//! The bridge to NetSurf: [`HttpFetcher`] as `xui_netsurf`'s `Fetcher`, with
//! its `FetchResponder` as our [`Sink`].

use std::sync::Arc;

use xui_netsurf::{FetchMethod, FetchRequest, FetchResponder, Fetcher};

use super::{HttpFetcher, Method, Options, Request, Sink};

impl Sink for FetchResponder {
    fn status(&self, code: u16) {
        FetchResponder::status(self, code);
    }

    fn header(&self, name: &str, value: &str) {
        FetchResponder::header(self, name, value);
    }

    fn data(&self, bytes: &[u8]) {
        FetchResponder::data(self, bytes);
    }

    fn finish(self) {
        FetchResponder::finish(self);
    }

    fn fail(self, message: &str) {
        FetchResponder::fail(self, message);
    }

    fn is_aborted(&self) -> bool {
        FetchResponder::is_aborted(self)
    }
}

impl Fetcher for HttpFetcher {
    fn fetch(&self, request: FetchRequest, responder: FetchResponder) {
        let method = match request.method {
            FetchMethod::Get => Method::Get,
            FetchMethod::Head => Method::Head,
            FetchMethod::Post => Method::Post,
        };
        let request = Request {
            // A wiki page is asked for in the skin NetSurf lays out well.
            url: crate::sites::fetch_url(&request.url).into_owned(),
            method,
            headers: request.headers,
            body: request.body,
        };
        // xui-netsurf calls this on a thread of its own per request.
        self.fetch_here(request, responder);
    }
}

/// Makes NetSurf fetch `http:` and `https:` through a new [`HttpFetcher`].
/// Call once, before the first view opens.
pub fn install(options: Options) {
    xui_netsurf::set_fetcher(Arc::new(HttpFetcher::new(options)));
}
