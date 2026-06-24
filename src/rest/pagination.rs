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

use std::fmt;
use std::future::Future;
use std::pin::Pin;

use futures_core::Stream;
use serde::{Deserialize, Serialize};

use crate::Result;

/// An opaque pagination cursor returned by a list endpoint.
///
/// Cursors are produced by the server and must be passed back verbatim to
/// fetch the following page; their contents are an implementation detail.
/// Time-windowed endpoints surface their next time bound through this same
/// type, so callers never need to special-case the two pagination styles.
///
/// `Cursor` is `Serialize`/`Deserialize` so a caller can persist one (e.g. to a
/// database or job state) and later resume from it via
/// [`Paginator::starting_after`] without round-tripping through
/// [`as_str`](Self::as_str) / [`into_inner`](Self::into_inner) by hand.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
///
/// `#[non_exhaustive]`: this is expected to grow fields (e.g. time-window bounds
/// or a sort direction), so match it with `..` and read fields by name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct PageRequest {
    /// Cursor for the page to fetch, or `None` for the first page.
    pub cursor: Option<Cursor>,
    /// Maximum number of items to return, if a page size was configured.
    pub limit: Option<u32>,
}

/// A single page returned by a list endpoint.
///
/// `#[non_exhaustive]`: build one with [`Page::new`] rather than a struct
/// literal, so future fields (e.g. a total count) don't break callers.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
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
    max_pages: Option<usize>,
    pages_fetched: usize,
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
            max_pages: None,
            pages_fetched: 0,
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

    /// Cap the number of pages this paginator will fetch.
    ///
    /// At most `max` pages (hence requests) are fetched; once that many have
    /// been returned the paginator stops as if it had reached the final page,
    /// even if the server is still handing back a next cursor. `max_pages(0)`
    /// fetches nothing. This is a safety bound against a misbehaving backend that
    /// never terminates; the [repeated-cursor guard](Self::next_page) already
    /// covers a server that keeps echoing the *same* cursor, but `max_pages` also
    /// bounds one that keeps advancing without end.
    pub fn max_pages(mut self, max: usize) -> Self {
        self.max_pages = Some(max);
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
    ///
    /// Termination is guarded against a misbehaving backend: if the server
    /// returns a next cursor equal to the one just requested — a stuck server, a
    /// time bound that fails to advance, or a cursor that round-trips to the same
    /// window — the paginator returns this page and then stops rather than
    /// re-issuing the identical request forever. A [`max_pages`](Self::max_pages)
    /// cap, if set, bounds paging even when the cursor keeps advancing.
    pub async fn next_page(&mut self) -> Result<Option<Page<T>>> {
        // Checked before fetching so a `max_pages` cap issues *at most* that many
        // requests — `max_pages(0)` fetches nothing at all.
        if self.done || self.max_pages == Some(self.pages_fetched) {
            return Ok(None);
        }

        let requested = self.next_cursor.take();
        let req = PageRequest {
            cursor: requested.clone(),
            limit: self.page_size,
        };
        let page = (self.fetch)(req).await?;
        self.pages_fetched += 1;

        match &page.next_cursor {
            // Server handed back the same cursor it was given: refuse to spin.
            Some(next) if Some(next) == requested.as_ref() => self.done = true,
            Some(next) => self.next_cursor = Some(next.clone()),
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
    /// still carry a next cursor are skipped transparently.
    ///
    /// The stream ends at the first error. It is **fused**: after it yields
    /// `None` (exhausted) or an `Err`, every later poll returns `None`, so it
    /// composes safely with combinators that may poll past completion. On error
    /// the paginator is consumed with it, so there is no resume-after-error;
    /// rebuild a fresh paginator with [`starting_after`](Self::starting_after)
    /// from the last successfully returned page's cursor to continue.
    pub fn into_stream(self) -> impl Stream<Item = Result<T>> + Send
    where
        T: Send + 'static,
    {
        use futures_util::stream::StreamExt;
        futures_util::stream::try_unfold(
            (self, Vec::<T>::new().into_iter()),
            |(mut pager, mut items)| async move {
                loop {
                    if let Some(item) = items.next() {
                        return Ok(Some((item, (pager, items))));
                    }
                    match pager.next_page().await? {
                        Some(page) => items = page.items.into_iter(),
                        None => return Ok(None),
                    }
                }
            },
        )
        // `try_unfold` already stops producing after `None`/`Err`; `.fuse()` makes
        // that a guarantee (`FusedStream`) so downstream combinators behave.
        .fuse()
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
            Err(crate::TransientError::RateLimited { retry_after: None }.into())
        });
        let mut stream = Box::pin(pager.into_stream());
        let first = stream.next().await.unwrap();
        assert!(matches!(first, Err(Error::Transient(_))));
    }

    /// Build a paginator whose closure errors after `ok_pages` successful pages.
    fn errors_after(ok_pages: usize, calls: Arc<AtomicUsize>) -> Paginator<u64> {
        Paginator::new(move |_req: PageRequest| {
            let calls = Arc::clone(&calls);
            async move {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                if n < ok_pages {
                    // Hand back an advancing cursor so paging continues into the
                    // error (and isn't short-circuited by the repeated-cursor guard).
                    Ok::<_, Error>(Page::new(
                        vec![n as u64],
                        Some(Cursor::new((n + 1).to_string())),
                    ))
                } else {
                    Err(crate::TransientError::Unavailable {
                        status: 500,
                        message: "kaboom".into(),
                    }
                    .into())
                }
            }
        })
    }

    #[tokio::test]
    async fn errors_propagate_through_next_page() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut pager = errors_after(1, Arc::clone(&calls));

        // First page succeeds.
        let p1 = pager.next_page().await.unwrap().unwrap();
        assert_eq!(p1.items, vec![0]);
        // Second fetch errors and surfaces through next_page.
        assert!(matches!(pager.next_page().await, Err(Error::Transient(_))));
    }

    #[tokio::test]
    async fn errors_propagate_through_all() {
        let calls = Arc::new(AtomicUsize::new(0));
        let pager = errors_after(2, calls);
        assert!(matches!(pager.all().await, Err(Error::Transient(_))));
    }

    #[tokio::test]
    async fn empty_final_first_page_terminates_immediately() {
        // Server returns [] with no next cursor on the very first page.
        let calls = Arc::new(AtomicUsize::new(0));
        let calls2 = Arc::clone(&calls);
        let make = move || {
            let calls = Arc::clone(&calls2);
            Paginator::<u64>::new(move |_req: PageRequest| {
                let calls = Arc::clone(&calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, Error>(Page::new(vec![], None))
                }
            })
        };

        assert_eq!(make().all().await.unwrap(), Vec::<u64>::new());

        let collected: Vec<u64> = make().into_stream().map(|r| r.unwrap()).collect().await;
        assert_eq!(collected, Vec::<u64>::new());
        // One fetch for each of the two runs above — no spinning on the empty page.
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn repeated_cursor_does_not_spin() {
        // Pathological server: always returns the same non-advancing cursor.
        let calls = Arc::new(AtomicUsize::new(0));
        let calls2 = Arc::clone(&calls);
        let mut pager = Paginator::<u64>::new(move |_req: PageRequest| {
            let calls = Arc::clone(&calls2);
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, Error>(Page::new(vec![1u64], Some(Cursor::new("stuck"))))
            }
        })
        .starting_after("stuck");

        // First page comes back...
        let p1 = pager.next_page().await.unwrap().unwrap();
        assert_eq!(p1.items, vec![1]);
        // ...then the paginator refuses to re-issue the identical request.
        assert!(pager.next_page().await.unwrap().is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn repeated_cursor_terminates_all() {
        // `all()` must terminate even though the server never stops paging.
        let pager = Paginator::<u64>::new(move |req: PageRequest| async move {
            // Same cursor every time, regardless of what was requested.
            let _ = req;
            Ok::<_, Error>(Page::new(vec![7u64], Some(Cursor::new("loop"))))
        })
        .starting_after("loop");
        assert_eq!(pager.all().await.unwrap(), vec![7]);
    }

    #[tokio::test]
    async fn max_pages_caps_paging() {
        let calls = Arc::new(AtomicUsize::new(0));
        // 100 items, 2 per page would be 50 pages — cap it at 3.
        let pager = fake_endpoint(100, 2, Arc::clone(&calls)).max_pages(3);
        let items = pager.all().await.unwrap();
        assert_eq!(items, vec![0, 1, 2, 3, 4, 5]);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn max_pages_zero_fetches_nothing() {
        let calls = Arc::new(AtomicUsize::new(0));
        let pager = fake_endpoint(100, 2, Arc::clone(&calls)).max_pages(0);
        assert_eq!(pager.all().await.unwrap(), Vec::<u64>::new());
        // The cap is checked before fetching, so no request is ever issued.
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn into_stream_is_fused_after_completion() {
        let calls = Arc::new(AtomicUsize::new(0));
        let pager = fake_endpoint(3, 2, Arc::clone(&calls));
        let mut stream = Box::pin(pager.into_stream());

        let mut got = Vec::new();
        while let Some(item) = stream.next().await {
            got.push(item.unwrap());
        }
        assert_eq!(got, vec![0, 1, 2]);

        // Polling past exhaustion keeps returning `None` (fused) — no re-fetch.
        assert!(stream.next().await.is_none());
        assert!(stream.next().await.is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn into_stream_is_fused_after_error() {
        let calls = Arc::new(AtomicUsize::new(0));
        // Errors on the very first fetch.
        let pager = errors_after(0, Arc::clone(&calls));
        let mut stream = Box::pin(pager.into_stream());

        assert!(matches!(
            stream.next().await,
            Some(Err(Error::Transient(_)))
        ));
        // After the error the fused stream yields `None`, not another fetch.
        assert!(stream.next().await.is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn cursor_serde_round_trips() {
        let cursor = Cursor::new("opaque-token");
        let json = serde_json::to_string(&cursor).unwrap();
        assert_eq!(json, "\"opaque-token\"");
        let back: Cursor = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cursor);
    }
}
