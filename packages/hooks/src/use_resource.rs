#![allow(missing_docs)]

use crate::{use_callback, use_signal, UseCallback};
use dioxus_core::prelude::*;
use dioxus_core::{
    prelude::{spawn, use_hook},
    Task,
};
use dioxus_signals::*;
use futures_util::{future, pin_mut, FutureExt, StreamExt};
use std::ops::Deref;
use std::{cell::Cell, future::Future, rc::Rc};

/// A memo that resolve to a value asynchronously.
/// Unlike `use_future`, `use_resource` runs on the **server**
/// See [`Resource`] for more details.
/// ```rust
///fn app() -> Element {
///    let country = use_signal(|| WeatherLocation {
///        city: "Berlin".to_string(),
///        country: "Germany".to_string(),
///        coordinates: (52.5244, 13.4105)
///    });
///
///    let current_weather = //run a future inside the use_resource hook
///        use_resource(move || async move { get_weather(&country.read().clone()).await });
///    
///    rsx! {
///        //the value of the future can be polled to
///        //conditionally render elements based off if the future
///        //finished (Some(Ok(_)), errored Some(Err(_)),
///        //or is still finishing (None)
///        match current_weather.value() {
///            Some(Ok(weather)) => WeatherElement { weather },
///            Some(Err(e)) => p { "Loading weather failed, {e}" }
///            None =>  p { "Loading..." }
///        }
///    }
///}
/// ```
#[must_use = "Consider using `cx.spawn` to run a future without reading its value"]
pub fn use_resource<T, F>(future: impl Fn() -> F + 'static) -> Resource<T>
where
    T: 'static,
    F: Future<Output = T> + 'static,
{
    let mut value = use_signal(|| None);
    let mut state = use_signal(|| UseResourceState::Pending);
    let (rc, changed) = use_hook(|| {
        let (rc, changed) = ReactiveContext::new();
        (rc, Rc::new(Cell::new(Some(changed))))
    });

    let cb = use_callback(move || {
        // Create the user's task
        #[allow(clippy::redundant_closure)]
        let fut = rc.run_in(|| future());

        // Spawn a wrapper task that polls the innner future and watch its dependencies
        spawn(async move {
            // move the future here and pin it so we can poll it
            let fut = fut;
            pin_mut!(fut);

            // Run each poll in the context of the reactive scope
            // This ensures the scope is properly subscribed to the future's dependencies
            let res = future::poll_fn(|cx| rc.run_in(|| fut.poll_unpin(cx))).await;

            // Set the value and state
            state.set(UseResourceState::Ready);
            value.set(Some(res));
        })
    });

    let mut task = use_hook(|| Signal::new(cb()));

    use_hook(|| {
        let mut changed = changed.take().unwrap();
        spawn(async move {
            loop {
                // Wait for the dependencies to change
                let _ = changed.next().await;

                // Stop the old task
                task.write().cancel();

                // Start a new task
                task.set(cb());
            }
        })
    });

    Resource {
        task,
        value,
        state,
        callback: cb,
    }
}

#[allow(unused)]
pub struct Resource<T: 'static> {
    value: Signal<Option<T>>,
    task: Signal<Task>,
    state: Signal<UseResourceState>,
    callback: UseCallback<Task>,
}

/// A signal that represents the state of a future
// we might add more states (panicked, etc)
#[derive(Clone, Copy, PartialEq, Hash, Eq, Debug)]
pub enum UseResourceState {
    /// The future is still running
    Pending,

    /// The future has been forcefully stopped
    Stopped,

    /// The future has been paused, tempoarily
    Paused,

    /// The future has completed
    Ready,
}

impl<T> Resource<T> {
    /// Restart the future with new dependencies.
    ///
    /// Will not cancel the previous future, but will ignore any values that it
    /// generates.
    pub fn restart(&mut self) {
        self.task.write().cancel();
        let new_task = self.callback.call();
        self.task.set(new_task);
    }

    /// Forcefully cancel a future
    pub fn cancel(&mut self) {
        self.state.set(UseResourceState::Stopped);
        self.task.write().cancel();
    }

    /// Pause the future
    pub fn pause(&mut self) {
        self.state.set(UseResourceState::Paused);
        self.task.write().pause();
    }

    /// Resume the future
    pub fn resume(&mut self) {
        if self.finished() {
            return;
        }

        self.state.set(UseResourceState::Pending);
        self.task.write().resume();
    }

    /// Get a handle to the inner task backing this future
    /// Modify the task through this handle will cause inconsistent state
    pub fn task(&self) -> Task {
        self.task.cloned()
    }

    /// Is the future currently finished running?
    ///
    /// Reading this does not subscribe to the future's state
    pub fn finished(&self) -> bool {
        matches!(
            *self.state.peek(),
            UseResourceState::Ready | UseResourceState::Stopped
        )
    }

    /// Get the current state of the future.
    pub fn state(&self) -> ReadOnlySignal<UseResourceState> {
        self.state.into()
    }

    /// Get the current value of the future.
    pub fn value(&self) -> ReadOnlySignal<Option<T>> {
        self.value.into()
    }
}

impl<T> From<Resource<T>> for ReadOnlySignal<Option<T>> {
    fn from(val: Resource<T>) -> Self {
        val.value.into()
    }
}

impl<T> Readable for Resource<T> {
    type Target = Option<T>;
    type Storage = UnsyncStorage;

    #[track_caller]
    fn try_read_unchecked(
        &self,
    ) -> Result<ReadableRef<'static, Self>, generational_box::BorrowError> {
        self.value.try_read_unchecked()
    }

    #[track_caller]
    fn peek_unchecked(&self) -> ReadableRef<'static, Self> {
        self.value.peek_unchecked()
    }
}

impl<T> IntoAttributeValue for Resource<T>
where
    T: Clone + IntoAttributeValue,
{
    fn into_value(self) -> dioxus_core::AttributeValue {
        self.with(|f| f.clone().into_value())
    }
}

impl<T> IntoDynNode for Resource<T>
where
    T: Clone + IntoDynNode,
{
    fn into_dyn_node(self) -> dioxus_core::DynamicNode {
        self().into_dyn_node()
    }
}

/// Allow calling a signal with signal() syntax
///
/// Currently only limited to copy types, though could probably specialize for string/arc/rc
impl<T: Clone> Deref for Resource<T> {
    type Target = dyn Fn() -> Option<T>;

    fn deref(&self) -> &Self::Target {
        Readable::deref_impl(self)
    }
}
