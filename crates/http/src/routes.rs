// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Route registration with an access marker. Every route is added with
//! [`RouteExt::route_as`], which records what kind of access control it
//! has; a security test enumerates [`route_table`] and checks each marker's
//! promise against the running server. Plain `Router::route` is not used
//! anywhere else in this crate (the same test scans the sources).

use std::collections::BTreeMap;
use std::sync::Mutex;

use axum::Router;
use axum::routing::MethodRouter;

/// What a route promises about who may use it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Access {
    /// Anyone, with any method. Serves nothing private.
    Public,
    /// Anyone may read (GET/HEAD); every other method needs an
    /// authorized subject.
    PublicRead,
    /// The signed-in user's own account; anonymous requests never succeed.
    Account,
    /// A namespace, organization, or repository authorized by the service
    /// layer. Subjects who can't see it get exactly what a missing one gets.
    Resource,
    /// Git smart HTTP, authorized per repository.
    Git,
    /// Site administrators with a browser session; everyone else gets 404.
    SiteAdmin,
}

static REGISTRY: Mutex<BTreeMap<&'static str, Access>> = Mutex::new(BTreeMap::new());

pub trait RouteExt {
    /// `Router::route` plus the route's access marker.
    #[must_use]
    fn route_as(
        self,
        path: &'static str,
        access: Access,
        method_router: Self::MethodRouter,
    ) -> Self;
    type MethodRouter;
}

impl<S: Clone + Send + Sync + 'static> RouteExt for Router<S> {
    type MethodRouter = MethodRouter<S>;

    #[allow(clippy::disallowed_methods)]
    fn route_as(self, path: &'static str, access: Access, method_router: MethodRouter<S>) -> Self {
        if let Ok(mut r) = REGISTRY.lock() {
            r.insert(path, access);
        }
        self.route(path, method_router)
    }
}

/// Every route of the HTTP server (pages, API, Git, probes) with its marker.
#[must_use]
pub fn route_table() -> Vec<(&'static str, Access)> {
    let _ = crate::ui::router();
    let _ = crate::api::router();
    let _ = crate::githttp::router();
    let _ = crate::probes();
    let _ = crate::metrics::router(std::sync::Arc::default());
    REGISTRY
        .lock()
        .map(|r| r.iter().map(|(p, a)| (*p, *a)).collect())
        .unwrap_or_default()
}
