// Copyright 2015-2018 Parity Technologies (UK) Ltd.
// This file is part of Parity.

// Parity is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Parity is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Parity.  If not, see <http://www.gnu.org/licenses/>.

use futures::future::{self, Loop};
use futures::sync::{mpsc, oneshot};
use futures::{self, Future, Async, Sink, Stream};
use hyper::header::{LOCATION, CONTENT_LENGTH, CONTENT_TYPE, HeaderValue};
use hyper::StatusCode;
use hyper::{self, Method};
use hyper_rustls;
use bytes::Bytes;
use std;
use std::cmp::min;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::thread;
use std::time::Duration;
use std::{io, fmt};
use tokio;
use tokio_timer::{self, Timer};
use url::{self, Url};

const MAX_SIZE: usize = 64 * 1024 * 1024;
const MAX_SECS: Duration = Duration::from_secs(5);
const MAX_REDR: usize = 5;

/// A handle to abort requests.
///
/// Requests are either aborted based on reaching thresholds such as
/// maximum response size, timeouts or too many redirects, or else
/// they can be aborted explicitly by the calling code.
#[derive(Clone, Debug)]
pub struct Abort {
	abort: Arc<AtomicBool>,
	size: usize,
	time: Duration,
	redir: usize,
}

impl Default for Abort {
	fn default() -> Abort {
		Abort {
			abort: Arc::new(AtomicBool::new(false)),
			size: MAX_SIZE,
			time: MAX_SECS,
			redir: MAX_REDR,
		}
	}
}

impl From<Arc<AtomicBool>> for Abort {
	fn from(a: Arc<AtomicBool>) -> Abort {
		Abort {
			abort: a,
			size: MAX_SIZE,
			time: MAX_SECS,
			redir: MAX_REDR,
		}
	}
}

impl Abort {
	/// True if `abort` has been invoked.
	pub fn is_aborted(&self) -> bool {
		self.abort.load(Ordering::SeqCst)
	}

	/// The maximum response body size.
	pub fn max_size(&self) -> usize {
		self.size
	}

	/// The maximum total time, including redirects.
	pub fn max_duration(&self) -> Duration {
		self.time
	}

	/// The maximum number of redirects to allow.
	pub fn max_redirects(&self) -> usize {
		self.redir
	}

	/// Mark as aborted.
	pub fn abort(&self) {
		self.abort.store(true, Ordering::SeqCst)
	}

	/// Set the maximum reponse body size.
	pub fn with_max_size(self, n: usize) -> Abort {
		Abort { size: n, .. self }
	}

	/// Set the maximum duration (including redirects).
	pub fn with_max_duration(self, d: Duration) -> Abort {
		Abort { time: d, .. self }
	}

	/// Set the maximum number of redirects to follow.
	pub fn with_max_redirects(self, n: usize) -> Abort {
		Abort { redir: n, .. self }
	}
}

/// Types which retrieve content from some URL.
pub trait Fetch: Clone + Send + Sync + 'static {
	/// The result future.
	type Result: Future<Item=Response, Error=Error> + Send + 'static;

	/// Make a request to given URL
	fn fetch(&self, request: Request, abort: Abort) -> Self::Result;

	/// Get content from some URL.
	fn get(&self, url: &str, abort: Abort) -> Self::Result;

	/// Post content to some URL.
	fn post(&self, url: &str, abort: Abort) -> Self::Result;
}

type TxResponse = oneshot::Sender<Result<Response, Error>>;
type TxStartup = std::sync::mpsc::SyncSender<Result<(), io::Error>>;
type ChanItem = Option<(Request, Abort, TxResponse)>;

/// An implementation of `Fetch` using a `hyper` client.
// Due to the `Send` bound of `Fetch` we spawn a background thread for
// actual request/response processing as `hyper::Client` itself does
// not implement `Send` currently.
#[derive(Debug)]
pub struct Client {
	core: mpsc::Sender<ChanItem>,
	refs: Arc<AtomicUsize>,
	timer: Timer,
}

// When cloning a client we increment the internal reference counter.
impl Clone for Client {
	fn clone(&self) -> Client {
		self.refs.fetch_add(1, Ordering::SeqCst);
		Client {
			core: self.core.clone(),
			refs: self.refs.clone(),
			timer: self.timer.clone(),
		}
	}
}

// When dropping a client, we decrement the reference counter.
// Once it reaches 0 we terminate the background thread.
impl Drop for Client {
	fn drop(&mut self) {
		if self.refs.fetch_sub(1, Ordering::SeqCst) == 1 {
			// ignore send error as it means the background thread is gone already
			let _ = self.core.clone().send(None).wait();
		}
	}
}

impl Client {
	/// Create a new fetch client.
	pub fn new() -> Result<Self, Error> {
		let (tx_start, rx_start) = std::sync::mpsc::sync_channel(1);
		let (tx_proto, rx_proto) = mpsc::channel(64);

		Client::background_thread(tx_start, rx_proto)?;

		match rx_start.recv_timeout(Duration::from_secs(10)) {
			Err(RecvTimeoutError::Timeout) => {
				error!(target: "fetch", "timeout starting background thread");
				return Err(Error::BackgroundThreadDead)
			}
			Err(RecvTimeoutError::Disconnected) => {
				error!(target: "fetch", "background thread gone");
				return Err(Error::BackgroundThreadDead)
			}
			Ok(Err(e)) => {
				error!(target: "fetch", "error starting background thread: {}", e);
				return Err(e.into())
			}
			Ok(Ok(())) => {}
		}

		Ok(Client {
			core: tx_proto,
			refs: Arc::new(AtomicUsize::new(1)),
			timer: Timer::default(),
		})
	}

	fn background_thread(tx_start: TxStartup, rx_proto: mpsc::Receiver<ChanItem>) -> io::Result<thread::JoinHandle<()>> {
		thread::Builder::new().name("fetch".into()).spawn(move || {
			let https_connector= hyper_rustls::HttpsConnector::new(4);
			let hyper = hyper::Client::builder().build(https_connector);


			let future = rx_proto.take_while(|item| Ok(item.is_some()))
				.map(|item| item.expect("`take_while` is only passing on channel items != None; qed"))
				.for_each(move |(request, abort, sender)|
			{
				trace!(target: "fetch", "new request to {}", request.url());
				if abort.is_aborted() {
					return future::ok(sender.send(Err(Error::Aborted)).unwrap_or(()))
				}
				let ini = (hyper.clone(), request, abort, 0);
				let fut = future::loop_fn(ini, |(client, request, abort, redirects)| {
					let request2 = request.clone();
					let url2 = request2.url().clone();
					let abort2 = abort.clone();
					client.request(request.into())
						.map(move |resp| Response::new(url2, resp, abort2))
						.from_err()
						.and_then(move |resp| {
							if abort.is_aborted() {
								debug!(target: "fetch", "fetch of {} aborted", request2.url());
								return Err(Error::Aborted)
							}
							if let Some((next_url, preserve_method)) = redirect_location(request2.url().clone(), &resp) {
								if redirects >= abort.max_redirects() {
									return Err(Error::TooManyRedirects)
								}
								let request = if preserve_method {
									let mut request2 = request2.clone();
									request2.set_url(next_url);
									request2
								} else {
									Request::new(next_url, Method::GET)
								};
								Ok(Loop::Continue((client, request, abort, redirects + 1)))
							} else {
								{
									if let Some(ref header_value) = resp.headers.get(CONTENT_LENGTH) {
										let content_len = header_value
											.to_str().map_err(|e| Error::ContentLengthToStrFailed(e))?
											.parse::<u64>().map_err(|e| Error::ContentLengthParseFailed(e))?;

										if content_len > abort.max_size() as u64 {
											return Err(Error::SizeLimit)
										}
									}
								}
								Ok(Loop::Break(resp))
							}
						})
					})
					.then(|result| {
						future::ok(sender.send(result).unwrap_or(()))
					});
				tokio::spawn(fut);
				trace!(target: "fetch", "waiting for next request ...");
				future::ok(())
			});

			tx_start.send(Ok(())).unwrap_or(());

			debug!(target: "fetch", "processing requests ...");
			tokio::run(future);
			debug!(target: "fetch", "fetch background thread finished")
		})
	}
}

impl Fetch for Client {
	type Result = Box<Future<Item=Response, Error=Error> + Send>;

	fn fetch(&self, request: Request, abort: Abort) -> Self::Result {
		debug!(target: "fetch", "fetching: {:?}", request.url());
		if abort.is_aborted() {
			return Box::new(future::err(Error::Aborted))
		}
		let (tx_res, rx_res) = oneshot::channel();
		let maxdur = abort.max_duration();
		let sender = self.core.clone();
		let future = sender.send(Some((request, abort, tx_res)))
			.map_err(|e| {
				error!(target: "fetch", "failed to schedule request: {}", e);
				Error::BackgroundThreadDead
			})
			.and_then(|_| rx_res.map_err(|oneshot::Canceled| Error::BackgroundThreadDead))
			.and_then(future::result);

		Box::new(self.timer.timeout(future, maxdur))
	}

	/// Get content from some URL.
	fn get(&self, url: &str, abort: Abort) -> Self::Result {
		let url: Url = match url.parse() {
			Ok(u) => u,
			Err(e) => return Box::new(future::err(e.into()))
		};
		self.fetch(Request::get(url), abort)
	}

	/// Post content to some URL.
	fn post(&self, url: &str, abort: Abort) -> Self::Result {
		let url: Url = match url.parse() {
			Ok(u) => u,
			Err(e) => return Box::new(future::err(e.into()))
		};
		self.fetch(Request::post(url), abort)
	}
}

// Extract redirect location from response. The second return value indicate whether the original method should be preserved.
fn redirect_location(u: Url, r: &Response) -> Option<(Url, bool)> {
	use hyper::StatusCode;

	let preserve_method = match r.status() {
		StatusCode::TEMPORARY_REDIRECT | StatusCode::PERMANENT_REDIRECT => true,
		_ => false,
	};
	match r.status() {
		StatusCode::MOVED_PERMANENTLY
		| StatusCode::PERMANENT_REDIRECT
		| StatusCode::TEMPORARY_REDIRECT
		| StatusCode::FOUND
		| StatusCode::SEE_OTHER => {
			// TODO: Handle unwrap
			if let Some(loc) = r.headers.get(LOCATION) {
				u.join(loc.to_str().unwrap()).ok().map(|url| (url, preserve_method))
			} else {
				None
			}
		}
		_ => None
	}
}

/// A wrapper for hyper::Request using Url and with methods.
#[derive(Debug, Clone)]
pub struct Request {
	url: Url,
	method: Method,
	headers: hyper::header::HeaderMap,
	body: Bytes
}

impl Request {
	/// Create a new request, with given url and method.
	pub fn new(url: Url, method: Method) -> Request {
		Request {
			url, method,
			headers: hyper::header::HeaderMap::new(),
			body: Default::default(),
		}
	}

	/// Create a new GET request.
	pub fn get(url: Url) -> Request {
		Request::new(url, Method::GET)
	}

	/// Create a new empty POST request.
	pub fn post(url: Url) -> Request {
		Request::new(url, Method::POST)
	}

	/// Read the url.
	pub fn url(&self) -> &Url {
		&self.url
	}

	/// Read the request headers.
	pub fn headers(&self) -> &hyper::header::HeaderMap {
		&self.headers
	}

	/// Get a mutable reference to the headers.
	pub fn headers_mut(&mut self) -> &mut hyper::header::HeaderMap {
		&mut self.headers
	}

	/// Set the body of the request.
	pub fn set_body<T: Into<Bytes>>(&mut self, body: T) {
		self.body = body.into();
	}

	/// Set the url of the request.
	pub fn set_url(&mut self, url: Url) {
		self.url = url;
	}

	/// Consume self, and return it with the added given header.
	pub fn with_header(mut self, name: hyper::header::HeaderName, value: hyper::header::HeaderValue) -> Self {
		self.headers_mut().append(name, value);
		self
	}

	/// Consume self, and return it with the body.
	pub fn with_body<T: Into<Bytes>>(mut self, body: T) -> Self {
		self.set_body(body);
		self
	}
}

impl From<Request> for hyper::Request<hyper::Body> {
	fn from(req: Request) -> hyper::Request<hyper::Body> {
		let uri: hyper::Uri = req.url.as_ref().parse().expect("Every valid URLis also a URI.");
		hyper::Request::builder()
			.method(req.method)
			.uri(uri)
			.header(hyper::header::USER_AGENT, HeaderValue::from_static("Parity Fetch Neo"))
			.body(req.body.into())
			.expect("Header, uri, method, and body are already valid and can not fail to parse; qed")
	}
}

/// An HTTP response.
#[derive(Debug)]
pub struct Response {
	url: Url,
	status: StatusCode,
	headers: hyper::header::HeaderMap,
	body: hyper::body::Body,
	abort: Abort,
	nread: usize,
}

impl Response {
	/// Create a new response, wrapping a hyper response.
	pub fn new(u: Url, r: hyper::Response<hyper::body::Body>, a: Abort) -> Response {
		Response {
			url: u,
			status: r.status(),
			headers: r.headers().clone(),
			body: r.into_body(),
			abort: a,
			nread: 0,
		}
	}

	/// The response status.
	pub fn status(&self) -> StatusCode {
		self.status
	}

	/// Status code == OK (200)?
	pub fn is_success(&self) -> bool {
		self.status() == StatusCode::OK
	}

	/// Status code == 404.
	pub fn is_not_found(&self) -> bool {
		self.status() == StatusCode::NOT_FOUND
	}

	/// Is the content-type text/html?
	pub fn is_html(&self) -> bool {
		if let Some(ref mime) = self.content_type() {
			mime == "text/html"
		} else {
			false
		}
	}

	/// The content-type header value.
	pub fn content_type(&self) -> Option<HeaderValue> {
		self.headers.get(CONTENT_TYPE).map(|r| r.clone())
	}
}

impl Stream for Response {
	type Item = hyper::Chunk;
	type Error = Error;

	fn poll(&mut self) -> futures::Poll<Option<Self::Item>, Self::Error> {
		if self.abort.is_aborted() {
			debug!(target: "fetch", "fetch of {} aborted", self.url);
			return Err(Error::Aborted)
		}
		match try_ready!(self.body.poll()) {
			None => Ok(Async::Ready(None)),
			Some(c) => {
				if self.nread + c.len() > self.abort.max_size() {
					debug!(target: "fetch", "size limit {:?} for {} exceeded", self.abort.max_size(), self.url);
					return Err(Error::SizeLimit)
				}
				self.nread += c.len();
				Ok(Async::Ready(Some(c)))
			}
		}
	}
}

/// `BodyReader` serves as an adapter from async to sync I/O.
///
/// It implements `io::Read` by repedately waiting for the next `Chunk`
/// of hyper's response `Body` which blocks the current thread.
pub struct BodyReader {
	chunk: hyper::Chunk,
	body: Option<hyper::Body>,
	abort: Abort,
	offset: usize,
	count: usize,
}

impl BodyReader {
	/// Create a new body reader for the given response.
	pub fn new(r: Response) -> BodyReader {
		BodyReader {
			body: Some(r.body),
			chunk: Default::default(),
			abort: r.abort,
			offset: 0,
			count: 0,
		}
	}
}

impl io::Read for BodyReader {
	fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
		let mut n = 0;
		while self.body.is_some() {
			// Can we still read from the current chunk?
			if self.offset < self.chunk.len() {
				let k = min(self.chunk.len() - self.offset, buf.len() - n);
				if self.count + k > self.abort.max_size() {
					debug!(target: "fetch", "size limit {:?} exceeded", self.abort.max_size());
					return Err(io::Error::new(io::ErrorKind::PermissionDenied, "size limit exceeded"))
				}
				let c = &self.chunk[self.offset .. self.offset + k];
				(&mut buf[n .. n + k]).copy_from_slice(c);
				self.offset += k;
				self.count += k;
				n += k;
				if n == buf.len() {
					break
				}
			} else {
				let body = self.body.take().expect("loop condition ensures `self.body` is always defined; qed");
				match body.into_future().wait() { // wait for next chunk
					Err((e, _)) => {
						error!(target: "fetch", "failed to read chunk: {}", e);
						return Err(io::Error::new(io::ErrorKind::Other, "failed to read body chunk"))
					}
					Ok((None, _)) => break, // body is exhausted, break out of the loop
					Ok((Some(c), b)) => {
						self.body = Some(b);
						self.chunk = c;
						self.offset = 0
					}
				}
			}
		}
		Ok(n)
	}
}

/// Fetch error cases.
#[derive(Debug)]
pub enum Error {
	/// Hyper gave us an error.
	Hyper(hyper::Error),
	/// Some I/O error occured.
	Io(io::Error),
	/// Invalid URLs where attempted to parse.
	Url(url::ParseError),
	/// Calling code invoked `Abort::abort`.
	Aborted,
	/// Too many redirects have been encountered.
	TooManyRedirects,
	/// tokio-timer gave us an error.
	Timer(tokio_timer::TimerError),
	/// The maximum duration was reached.
	Timeout,
	/// The response body is too large.
	SizeLimit,
	/// The background processing thread does not run.
	BackgroundThreadDead,
	/// The CONTENT_LENGTH header could not be converted to a number
	ContentLengthParseFailed(std::num::ParseIntError),
	/// The CONTENT_LENGTH header could not be converted to a string by hyper
	ContentLengthToStrFailed(hyper::header::ToStrError),
}

impl fmt::Display for Error {
	fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
		match *self {
			Error::Aborted => write!(fmt, "The request has been aborted."),
			Error::Hyper(ref e) => write!(fmt, "{}", e),
			Error::Url(ref e) => write!(fmt, "{}", e),
			Error::Io(ref e) => write!(fmt, "{}", e),
			Error::BackgroundThreadDead => write!(fmt, "background thread gone"),
			Error::TooManyRedirects => write!(fmt, "too many redirects"),
			Error::Timer(ref e) => write!(fmt, "{}", e),
			Error::Timeout => write!(fmt, "request timed out"),
			Error::SizeLimit => write!(fmt, "size limit reached"),
			Error::ContentLengthParseFailed(ref e) => write!(fmt, "{}", e),
			Error::ContentLengthToStrFailed(ref e) => write!(fmt, "{}", e),
		}
	}
}

impl From<hyper::Error> for Error {
	fn from(e: hyper::Error) -> Self {
		Error::Hyper(e)
	}
}

impl From<io::Error> for Error {
	fn from(e: io::Error) -> Self {
		Error::Io(e)
	}
}

impl From<url::ParseError> for Error {
	fn from(e: url::ParseError) -> Self {
		Error::Url(e)
	}
}

impl<F> From<tokio_timer::TimeoutError<F>> for Error {
	fn from(e: tokio_timer::TimeoutError<F>) -> Self {
		match e {
			tokio_timer::TimeoutError::Timer(_, e) => Error::Timer(e),
			tokio_timer::TimeoutError::TimedOut(_) => Error::Timeout,
		}
	}
}

#[cfg(test)]
mod test {
	use super::*;
	use hyper::{Response, StatusCode, Body, Server};
	use hyper::service::service_fn_ok;
	use std::net::SocketAddr;

	const ADDRESS: &str = "127.0.0.1:8080";
	fn start_test_server() {
		std::thread::spawn(|| {
		let new_service = || {
			service_fn_ok(|r| {
				let mut response = Response::new(Body::empty());
				match r.uri().path() {
					"/" => {
						let body = hyper::Body::from(r.uri().query().unwrap_or("").to_string());
						*response.body_mut() = body;
						*response.status_mut() = StatusCode::OK;
					}
					"/redirect" => {
						*response.status_mut() = StatusCode::MOVED_PERMANENTLY;
						let hv = HeaderValue::from_str(r.uri().query().unwrap_or("/")).unwrap();
						response.headers_mut().insert("LOCATION", hv);
					}
					"/loop" => {
						*response.status_mut() = StatusCode::MOVED_PERMANENTLY;
						let hv = HeaderValue::from_str("/loop").unwrap();
						response.headers_mut().insert("LOCATION", hv);
					}
					"/delay" => {
						let d = Duration::from_secs(r.uri().query().unwrap_or("0").parse().unwrap());
						std::thread::sleep(d);
						*response.body_mut() = Body::empty();
					}
					_ => {
						*response.status_mut() = StatusCode::NOT_FOUND;
					}
				};
				return response;
			})
		};

		let addr: SocketAddr = ADDRESS.parse().unwrap();

		let server = Server::try_bind(&addr);
		if !server.is_ok() {
			return;
		}
		let server = server.unwrap().serve(new_service).map_err(|e| eprintln!("server error: {}", e));
		hyper::rt::run(server);
	});
	}

	#[test]
	fn it_should_fetch() {
		start_test_server();
		let client = Client::new().unwrap();
		let mut runtime = tokio::runtime::Runtime::new().unwrap();
		let f = client.get(&format!("http://{}?123", ADDRESS), Default::default());
		let resp = runtime.block_on(f);
		let resp = resp.unwrap();
		let body = runtime.block_on(resp.concat2());
		assert!(body.is_ok());
		let body = body.unwrap();
		assert_eq!(&body[..], b"123");
		runtime.shutdown_now();
	}

	#[test]
	fn it_should_timeout() {
		start_test_server();
		let client = Client::new().unwrap();
		let abort = Abort::default().with_max_duration(Duration::from_secs(1));
		let mut runtime = tokio::runtime::Runtime::new().unwrap();
		let f = client.get(&format!("http://{}/delay?3", ADDRESS), abort);
		match runtime.block_on(f) {
			Err(Error::Timeout) => {}
			other => panic!("expected timeout, got {:?}", other)
		}
	}

	#[test]
	fn it_should_follow_redirects() {
		start_test_server();
		let client = Client::new().unwrap();
		let abort = Abort::default();
		let mut runtime = tokio::runtime::Runtime::new().unwrap();
		let f = client.get(&format!("http://{}/redirect?http://{}/", ADDRESS, ADDRESS), abort);
		let result = runtime.block_on(f);
		assert!(result.is_ok());
		runtime.shutdown_now();
	}

	#[test]
	fn it_should_follow_relative_redirects() {
		start_test_server();
		let client = Client::new().unwrap();
		let abort = Abort::default().with_max_redirects(4);
		let mut runtime = tokio::runtime::Runtime::new().unwrap();
		let f = client.get(&format!("http://{}/redirect?/", ADDRESS), abort);
		let result = runtime.block_on(f);
		assert!(result.is_ok());
		runtime.shutdown_now();
	}

	#[test]
	fn it_should_not_follow_too_many_redirects() {
		start_test_server();
		let client = Client::new().unwrap();
		let abort = Abort::default().with_max_redirects(3);
		let mut runtime = tokio::runtime::Runtime::new().unwrap();
		let f = client.get(&format!("http://{}/loop", ADDRESS), abort);
		let result = runtime.block_on(f);
		match result {
			Err(Error::TooManyRedirects) => {}
			other => panic!("expected too many redirects error, got {:?}", other)
		}
		runtime.shutdown_now();
	}

	#[test]
	fn it_should_read_data() {
		start_test_server();
		let client = Client::new().unwrap();
		let abort = Abort::default();
		let mut runtime = tokio::runtime::Runtime::new().unwrap();
		let f = client.get(&format!("http://{}?abcdefghijklmnopqrstuvwxyz", ADDRESS), abort);
		let result = runtime.block_on(f);
		assert!(result.is_ok());
		let result = result.unwrap();
		let body = runtime.block_on(result.concat2());
		assert!(body.is_ok());
		let body = body.unwrap();
		assert_eq!(&body[..], b"abcdefghijklmnopqrstuvwxyz");
		runtime.shutdown_now();
	}

	#[test]
	fn it_should_not_read_too_much_data() {
		start_test_server();
		let client = Client::new().unwrap();
		let abort = Abort::default().with_max_size(3);
		let mut runtime = tokio::runtime::Runtime::new().unwrap();
		let f = client.get(&format!("http://{}/?1234", ADDRESS), abort);
		let result = runtime.block_on(f);

		match result {
			Ok(_) => {},
			Err(e) => {
				match e {
					Error::SizeLimit => {},
					_ => { panic!("Expecting SizeLimit error, got {:#?}", e)}
				}
			}
		}
		runtime.shutdown_now();
	}
}
