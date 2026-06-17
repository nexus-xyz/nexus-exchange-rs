//! Cursor / time auto-paging for list endpoints.
//!
//! List endpoints accept a `limit` and return a page of results plus a cursor
//! pointing at the next page. Rather than make callers hold and re-submit that
//! cursor by hand (the way dYdX does), the SDK exposes a [`Paginator`] that
//! drives the cursor for you: ask it for the next [`Page`], iterate every item
//! with [`Paginator::all`], or consume it as a [`Stream`] via
//! [`Paginator::into_stream`].
//!
//! The paginator is generic over how a single page is fetched, so the same
//! machinery serves both cursor-based and time-windowed endpoints: a
//! time-windowed endpoint simply encodes its next time bound into the
//! [`Cursor`] it returns.
//!
//! ```
//! # use nexus_exchange::rest::pagination::{Cursor, Page, PageRequest, Paginator};
//! # use nexus_exchange::Result;
//! // A list endpoint method builds a `Paginator` from a closure that fetches
//! // one page for a given request. `Client` would capture itself here and
//! // issue the actual HTTP call.
//! fn list_trades() -> Paginator<u64> {
//!     Paginator::new(move |req: PageRequest| async move {
//!         // ... GET /v1/trades?limit={req.limit}&cursor={req.cursor} ...
//!         let _ = req;
//!         Ok::<_, nexus_exchange::Error>(Page::new(vec![1, 2, 3], None))
//!     })
//! }
//!
//! # async fn run() -> Result<()> {
//! let trades = list_trades().page_size(100).all().await?;
//! assert_eq!(trades, vec![1, 2, 3]);
//! # Ok(())
//! # }
//! ```

use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::pin::Pin;

use futures_core::Stream;

use crate::Result;

/// An opaque pagination cursor returned by a list endpoint.
///
/// Cursors are produced by the server and must be passed back verbatim to
/// fetch the following page; their contents are an implementation detail.
/// Time-windowed endpoints surface their next time bound through this same
/// type, so callers never need to special-case the two pagination styles.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Cursor(String);

impl Cursor {
    /// Wrap a raw cursor string (e.g. one previously persisted to resume from).
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    /// The cursor as a string slice, for use as a query parameter.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume the cursor, returning the underlying string.
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl From<String> for Cursor {
    fn from(raw: String) -> Self {
        Self(raw)
    }
}

impl From<&str> for Cursor {
    fn from(raw: &str) -> Self {
        Self(raw.to_owned())
    }
}

impl fmt::Display for Cursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The parameters for fetching a single page.
///
/// Passed to the closure given to [`Paginator::new`]; the endpoint method
/// translates it into query parameters on the underlying request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageRequest {
    /// Cursor for the page to fetch, or `None` for the first page.
    pub cursor: Option<Cursor>,
    /// Maximum number of items to return, if a page size was configured.
    pub limit: Option<u32>,
}

/// A single page returned by a list endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    /// The items in this page, in server order.
    pub items: Vec<T>,
    /// Cursor for the next page, or `None` when this is the final page.
    pub next_cursor: Option<Cursor>,
}

impl<T> Page<T> {
    /// Build a page from its items and optional next cursor.
    pub fn new(items: Vec<T>, next_cursor: Option<Cursor>) -> Self {
        Self { items, next_cursor }
    }

    /// Whether this is the last page (i.e. there is no next cursor).
    pub fn is_last(&self) -> bool {
        self.next_cursor.is_none()
    }
}

type PageFuture<T> = Pin<Box<dyn Future<Output = Result<Page<T>>> + Send>>;
type FetchFn<T> = Box<dyn FnMut(PageRequest) -> PageFuture<T> + Send>;

/// An auto-paging iterator over a list endpoint.
///
/// A `Paginator` holds the state needed to walk every page of a list endpoint,
/// advancing the cursor automatically. Drive it page-by-page with
/// [`next_page`](Self::next_page), collect everything with [`all`](Self::all),
/// or treat it as a [`Stream`] of items via [`into_stream`](Self::into_stream).
///
/// Pages are fetched lazily: no request is issued until the first page is
/// requested, and each subsequent page is fetched only when the previous one
/// has been consumed.
pub struct Paginator<T> {
    fetch: FetchFn<T>,
    next_cursor: Option<Cursor>,
    page_size: Option<u32>,
    done: bool,
}

impl<T> Paginator<T> {
    /// Build a paginator from a closure that fetches one page per request.
    ///
    /// The closure is called with a [`PageRequest`] carrying the cursor (and
    /// configured page size) for the page to fetch, and returns that page
    /// along with the cursor for the next one.
    pub fn new<F, Fut>(mut fetch: F) -> Self
    where
        T: 'static,
        F: FnMut(PageRequest) -> Fut + Send + 'static,
        Fut: Future<Output = Result<Page<T>>> + Send + 'static,
    {
        Self {
            fetch: Box::new(move |req| Box::pin(fetch(req))),
            next_cursor: None,
            page_size: None,
            done: false,
        }
    }

    /// Set the per-page `limit` requested from the endpoint.
    ///
    /// This bounds the size of each page, not the total number of items
    /// returned — the paginator still walks every page.
    pub fn page_size(mut self, limit: u32) -> Self {
        self.page_size = Some(limit);
        self
    }

    /// Resume paging from a previously obtained cursor.
    ///
    /// The next page fetched will be the one following `cursor`.
    pub fn starting_after(mut self, cursor: impl Into<Cursor>) -> Self {
        self.next_cursor = Some(cursor.into());
        self
    }

    /// Fetch the next page, or `None` once every page has been returned.
    ///
    /// Advances the internal cursor so the following call fetches the page
    /// after this one.
    pub async fn next_page(&mut self) -> Result<Option<Page<T>>> {
        if self.done {
            return Ok(None);
        }

        let req = PageRequest {
            cursor: self.next_cursor.take(),
            limit: self.page_size,
        };
        let page = (self.fetch)(req).await?;

        match &page.next_cursor {
            Some(cursor) => self.next_cursor = Some(cursor.clone()),
            None => self.done = true,
        }

        Ok(Some(page))
    }

    /// Walk every remaining page and collect all items into a single `Vec`.
    ///
    /// Convenience for the common "give me everything" case. Prefer
    /// [`next_page`](Self::next_page) or [`into_stream`](Self::into_stream)
    /// when the full result set may be large.
    pub async fn all(mut self) -> Result<Vec<T>> {
        let mut out = Vec::new();
        while let Some(page) = self.next_page().await? {
            out.extend(page.items);
        }
        Ok(out)
    }

    /// Consume the paginator as a [`Stream`] yielding one item at a time.
    ///
    /// Pages are fetched on demand as the stream is polled; empty pages that
    /// still carry a next cursor are skipped transparently. The stream stops at
    /// the first error.
    pub fn into_stream(self) -> impl Stream<Item = Result<T>> + Send
    where
        T: Send + 'static,
    {
        futures_util::stream::try_unfold(
            (self, VecDeque::<T>::new()),
            |(mut pager, mut buffer)| async move {
                loop {
                    if let Some(item) = buffer.pop_front() {
                        return Ok(Some((item, (pager, buffer))));
                    }
                    match pager.next_page().await? {
                        Some(page) => buffer.extend(page.items),
                        None => return Ok(None),
                    }
                }
            },
        )
    }
}

impl<T> fmt::Debug for Paginator<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Paginator")
            .field("next_cursor", &self.next_cursor)
            .field("page_size", &self.page_size)
            .field("done", &self.done)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Error;
    use futures_util::StreamExt;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    /// A fake endpoint that pages through `total` integers, `per_page` at a
    /// time, using the item index as an opaque cursor. Records how many pages
    /// (HTTP round-trips) were fetched.
    fn fake_endpoint(total: u64, per_page: u64, calls: Arc<AtomicUsize>) -> Paginator<u64> {
        Paginator::new(move |req: PageRequest| {
            let calls = Arc::clone(&calls);
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                let start: u64 = match &req.cursor {
                    Some(c) => c.as_str().parse().unwrap(),
                    None => 0,
                };
                let end = (start + per_page).min(total);
                let items: Vec<u64> = (start..end).collect();
                let next = (end < total).then(|| Cursor::new(end.to_string()));
                Ok::<_, Error>(Page::new(items, next))
            }
        })
    }

    #[tokio::test]
    async fn next_page_walks_every_page_then_returns_none() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut pager = fake_endpoint(5, 2, Arc::clone(&calls));

        let p1 = pager.next_page().await.unwrap().unwrap();
        assert_eq!(p1.items, vec![0, 1]);
        assert!(!p1.is_last());

        let p2 = pager.next_page().await.unwrap().unwrap();
        assert_eq!(p2.items, vec![2, 3]);

        let p3 = pager.next_page().await.unwrap().unwrap();
        assert_eq!(p3.items, vec![4]);
        assert!(p3.is_last());

        assert!(pager.next_page().await.unwrap().is_none());
        // No request is issued past the final page.
        assert!(pager.next_page().await.unwrap().is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn all_collects_items_in_order() {
        let calls = Arc::new(AtomicUsize::new(0));
        let pager = fake_endpoint(7, 3, Arc::clone(&calls));
        let items = pager.all().await.unwrap();
        assert_eq!(items, vec![0, 1, 2, 3, 4, 5, 6]);
        assert_eq!(calls.load(Ordering::SeqCst), 3); // 3+3+1
    }

    #[tokio::test]
    async fn into_stream_yields_every_item() {
        let calls = Arc::new(AtomicUsize::new(0));
        let pager = fake_endpoint(5, 2, calls);
        let collected: Vec<u64> = pager.into_stream().map(|r| r.unwrap()).collect().await;
        assert_eq!(collected, vec![0, 1, 2, 3, 4]);
    }

    #[tokio::test]
    async fn page_size_is_threaded_into_requests() {
        let seen = Arc::new(AtomicUsize::new(0));
        let seen2 = Arc::clone(&seen);
        let pager = Paginator::new(move |req: PageRequest| {
            let seen = Arc::clone(&seen2);
            async move {
                assert_eq!(req.limit, Some(50));
                seen.fetch_add(1, Ordering::SeqCst);
                Ok::<_, Error>(Page::new(vec![1u8], None))
            }
        })
        .page_size(50);
        pager.all().await.unwrap();
        assert_eq!(seen.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn starting_after_resumes_from_cursor() {
        let pager = fake_endpoint(5, 2, Arc::new(AtomicUsize::new(0))).starting_after("2");
        let items = pager.all().await.unwrap();
        assert_eq!(items, vec![2, 3, 4]);
    }

    #[tokio::test]
    async fn empty_page_with_cursor_is_skipped() {
        // Page 1: empty but has a next cursor; page 2: the real items.
        let pager = Paginator::new(move |req: PageRequest| async move {
            let page = match req.cursor.as_ref().map(Cursor::as_str) {
                None => Page::new(vec![], Some(Cursor::new("next"))),
                Some("next") => Page::new(vec![10u64, 11], None),
                other => panic!("unexpected cursor: {other:?}"),
            };
            Ok::<_, Error>(page)
        });
        assert_eq!(pager.all().await.unwrap(), vec![10, 11]);
    }

    #[tokio::test]
    async fn errors_propagate_and_halt_paging() {
        let pager = Paginator::<u64>::new(move |_req| async move {
            Err(Error::Api {
                code: "rate_limited".into(),
                message: "slow down".into(),
            })
        });
        let mut stream = Box::pin(pager.into_stream());
        let first = stream.next().await.unwrap();
        assert!(matches!(first, Err(Error::Api { .. })));
    }
}
