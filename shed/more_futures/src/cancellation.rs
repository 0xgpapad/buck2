/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

//! Defines a future with explicit cancellation

pub mod future;

use std::future::Future;

use futures::FutureExt;
use once_cell::sync::Lazy;

use crate::cancellable_future::critical_section;
use crate::cancellable_future::try_to_disable_cancellation;
use crate::cancellable_future::with_structured_cancellation;
use crate::cancellable_future::CancellationObserver;
use crate::cancellable_future::DisableCancellationGuard;
use crate::cancellation::future::ExecutionContext;

static INSTANCE: Lazy<CancellationContext> =
    Lazy::new(|| CancellationContext(CancellationContextInner::ThreadLocal));

/// Context available to the function running inside the future to control and manage it's own
/// cancellation
pub struct CancellationContext<'a>(CancellationContextInner<'a>);

impl<'a> CancellationContext<'a> {
    /// TODO replace with real initialization
    pub fn todo() -> Self {
        CancellationContext(CancellationContextInner::ThreadLocal)
    }

    pub fn testing() -> &'a Self {
        &INSTANCE
    }

    /// Create a new context from a thread that is never canceled
    pub fn never_cancelled() -> &'static Self {
        &INSTANCE
    }

    /// Enter a critical section during which the current future (if supports explicit cancellation)
    /// should not be dropped. If the future was not cancelled before entering the critical section,
    /// it becomes non-cancellable during the critical section. If it *was* cancelled before
    /// entering the critical section (i.e. the last ref was dropped during `poll`), then the
    /// future is allowed to continue executing until this future resolves.
    pub fn critical_section<F, Fut>(
        &'a self,
        make: F,
    ) -> impl Future<Output = <Fut as Future>::Output> + 'a
    where
        F: FnOnce() -> Fut + 'a,
        Fut: Future + 'a,
    {
        // placeholder to just delegate to existing critical_section code for now until we migrate
        // the callsites
        self.0.critical_section(make)
    }

    /// Enter a structured cancellation section. The caller receives a CancellationObserver. The
    /// CancellationObserver is a future that resolves when cancellation is requested (or when this
    /// section exits).
    pub fn with_structured_cancellation<F, Fut>(
        &'a self,
        make: F,
    ) -> impl Future<Output = <Fut as Future>::Output> + 'a
    where
        Fut: Future + 'a,
        F: FnOnce(CancellationObserver) -> Fut + 'a,
    {
        self.0.with_structured_cancellation(make)
    }

    /// For CancellableFutures futures, obtain a StrongRefCount for the current task and prevent
    /// cancellation while the guard is held.
    ///
    /// For ExplicitCancellationFutures, disables the cancellation of the future from here on out
    /// so that it will never end with canceled.
    ///
    /// This will return None if the task *is* within a cancellable future but has already been cancelled.
    pub fn try_to_disable_cancellation(&self) -> Option<DisableCancellationGuard> {
        self.0.try_to_disable_cancellation()
    }
}

/// Context available to only explicitly cancellable futures to manage their own cancellation
pub struct ExplicitCancellationContext {
    inner: ExecutionContext,
}

impl ExplicitCancellationContext {
    /// Enter a critical section during which the current future (if supports explicit cancellation)
    /// should not be dropped. If the future was not cancelled before entering the critical section,
    /// it becomes non-cancellable during the critical section. If it *was* cancelled before
    /// entering the critical section (i.e. the last ref was dropped during `poll`), then the
    /// future is allowed to continue executing until this future resolves.
    pub fn critical_section<'a, F, Fut>(
        &'a self,
        make: F,
    ) -> impl Future<Output = <Fut as Future>::Output> + 'a
    where
        F: FnOnce() -> Fut + 'a,
        Fut: Future + 'a,
    {
        let guard = self.inner.enter_critical_section();
        async move {
            let r = make().await;

            if guard.exit_prevent_cancellation() {
                // If the current future should actually be cancelled now, we try to return
                // control to it immediately to allow cancellation to kick in faster.
                tokio::task::yield_now().await;
            }

            r
        }
    }

    /// Enter a structured cancellation section. The caller receives a CancellationObserver. The
    /// CancellationObserver is a future that resolves when cancellation is requested (or when this
    /// section exits).
    pub fn with_structured_cancellation<'a, F, Fut>(
        &'a self,
        make: F,
    ) -> impl Future<Output = <Fut as Future>::Output> + 'a
    where
        Fut: Future + 'a,
        F: FnOnce(CancellationObserver) -> Fut + 'a,
    {
        let (observer, guard) = self.inner.enter_structured_cancellation();

        async move {
            let r = make(observer).await;

            if guard.exit_prevent_cancellation() {
                // If the current future should actually be cancelled now, we try to return
                // control to it immediately to allow cancellation to kick in faster.
                tokio::task::yield_now().await;
            }

            r
        }
    }

    /// Tries to disable the cancellation for this future from here on
    pub fn try_to_disable_cancellation(&self) -> Option<DisableCancellationGuard> {
        let (_, guard) = self.inner.enter_structured_cancellation();

        if guard.try_to_disable_cancellation() {
            Some(DisableCancellationGuard { _guard: None })
        } else {
            None
        }
    }

    pub fn into_compatible(&self) -> CancellationContext {
        CancellationContext(CancellationContextInner::Explicit(self))
    }

    pub fn testing<'a>() -> &'a Self {
        static INSTANCE: Lazy<ExplicitCancellationContext> =
            Lazy::new(|| ExplicitCancellationContext {
                inner: ExecutionContext::testing(),
            });

        &INSTANCE
    }
}

enum CancellationContextInner<'a> {
    /// The old thread local based implementation
    ThreadLocal,
    /// The cancellation context passed explicitly
    Explicit(&'a ExplicitCancellationContext),
}

impl<'a> CancellationContextInner<'a> {
    /// Enter a critical section during which the current future (if supports explicit cancellation)
    /// should not be dropped. If the future was not cancelled before entering the critical section,
    /// it becomes non-cancellable during the critical section. If it *was* cancelled before
    /// entering the critical section (i.e. the last ref was dropped during `poll`), then the
    /// future is allowed to continue executing until this future resolves.
    pub fn critical_section<F, Fut>(
        &'a self,
        make: F,
    ) -> impl Future<Output = <Fut as Future>::Output> + 'a
    where
        F: FnOnce() -> Fut + 'a,
        Fut: Future + 'a,
    {
        match self {
            CancellationContextInner::ThreadLocal => critical_section(make).left_future(),
            CancellationContextInner::Explicit(context) => {
                context.critical_section(make).right_future()
            }
        }
    }

    /// Enter a structured cancellation section. The caller receives a CancellationObserver. The
    /// CancellationObserver is a future that resolves when cancellation is requested (or when this
    /// section exits).
    pub fn with_structured_cancellation<F, Fut>(
        &'a self,
        make: F,
    ) -> impl Future<Output = <Fut as Future>::Output> + 'a
    where
        Fut: Future + 'a,
        F: FnOnce(CancellationObserver) -> Fut + 'a,
    {
        match self {
            CancellationContextInner::ThreadLocal => {
                with_structured_cancellation(make).left_future()
            }
            CancellationContextInner::Explicit(context) => {
                context.with_structured_cancellation(make).right_future()
            }
        }
    }

    /// Obtain a StrongRefCount for the current task. This will return None if the task *is* within a
    /// CancellableFuture but has already been cancelled.
    pub fn try_to_disable_cancellation(&self) -> Option<DisableCancellationGuard> {
        match self {
            CancellationContextInner::ThreadLocal => try_to_disable_cancellation(),
            CancellationContextInner::Explicit(context) => context.try_to_disable_cancellation(),
        }
    }
}
