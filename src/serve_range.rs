// © 2019 3D Robotics. License: Apache-2.0

use futures::Stream;
use hyper::{Request, Response, Body, StatusCode, header};
use crate::stream_range::{ Range, StreamRange };

/// Parse an HTTP range header to a `Range`
///
/// Returns Ok(Some(Range{..})) for a valid range, Ok(None) for a missing or unsupported range,
/// or Err(msg) if parsing fails.
pub fn parse_range(range_val: &str, total_len: u64) -> Result<Option<Range>, &'static str> {
    if !range_val.starts_with("bytes=") {
        return Err("invalid range unit");
    }

    let range_val = &range_val["bytes=".len()..].trim();

    if range_val.contains(",") {
        return Ok(None); // multiple ranges unsupported, but it's legal to just ignore the header
    }

    if range_val.starts_with("-") {
        let s = range_val[1..].parse::<u64>().map_err(|_| "invalid range number")?;
        
        if s >= total_len {
            return Ok(None);
        }

        Ok(Some(Range { start: total_len-s, end: total_len }))
    } else if range_val.ends_with("-") {
        let s = range_val[..range_val.len()-1].parse::<u64>().map_err(|_| "invalid range number")?;
        
        if s >= total_len {
            return Ok(None);
        }

        Ok(Some(Range { start: s, end: total_len}))
    } else if let Some(h) = range_val.find("-") {
        let s = range_val[..h].parse::<u64>().map_err(|_| "invalid range number")?;
        let e = range_val[h+1..].parse::<u64>().map_err(|_| "invalid range number")?;

        if e >= total_len || s > e {
            return Ok(None);
        }

        Ok(Some(Range { start: s, end: e+1 }))
    } else {
        return Err("invalid range");
    }
}

#[test]
fn test_range() {
    assert_eq!(parse_range("lines=0-10", 1000), Err("invalid range unit"));

    assert_eq!(parse_range("bytes=500-", 1000), Ok(Some(Range { start: 500, end: 1000})));
    assert_eq!(parse_range("bytes=2000-", 1000), Ok(None));
    
    assert_eq!(parse_range("bytes=-100", 1000), Ok(Some(Range { start: 900, end: 1000})));
    assert_eq!(parse_range("bytes=-2000", 1000), Ok(None));

    assert_eq!(parse_range("bytes=100-200", 1000), Ok(Some(Range { start: 100, end: 201})));
    assert_eq!(parse_range("bytes=500-999", 1000), Ok(Some(Range { start: 500, end: 1000})));
    assert_eq!(parse_range("bytes=500-1000", 1000), Ok(None));
    assert_eq!(parse_range("bytes=200-100", 1000), Ok(None));
    assert_eq!(parse_range("bytes=1500-2000", 1000), Ok(None));

    assert_eq!(parse_range("bytes=", 1000), Err("invalid range"));
    assert_eq!(parse_range("bytes=a-", 1000), Err("invalid range number"));
    assert_eq!(parse_range("bytes=a-b", 1000), Err("invalid range number"));
    assert_eq!(parse_range("bytes=-b", 1000), Err("invalid range number"));
}

/// Serve a `StreamRange` in response to a `hyper` request.
/// This handles the HTTP Range header and "206 Partial content" and associated headers if required
pub fn hyper_response(req: &Request<Body>, content_type: &str, etag: &str, filename: &str, data: &dyn StreamRange) -> Response<Body> {
    let full_len = data.len();
    let full_range = Range { start: 0, end: full_len };

    let range = req.headers().get(hyper::header::RANGE)
        .filter(|_| req.headers().get(hyper::header::IF_RANGE).map_or(true, |val| val == etag))
        .and_then(|v| v.to_str().ok())
        .and_then(|v| parse_range(v, full_len).ok())
        .and_then(|x| x);

    let mut res = Response::builder();
    res.header(header::CONTENT_TYPE, content_type);
    res.header(header::ACCEPT_RANGES, "bytes");
    res.header(header::ETAG, etag);
    res.header(header::CONTENT_DISPOSITION, format!("attachment; filename=\"{}\"", filename));

    if let Some(range) = range {
        res.status(StatusCode::PARTIAL_CONTENT);
        res.header(header::CONTENT_RANGE, format!("bytes {}-{}/{}", range.start, range.end - 1, full_len));
        log::info!("Serving range {:?}", range);
    }

    let range = range.unwrap_or(full_range);

    res.header(header::CONTENT_LENGTH, range.len());

    let stream = data.stream_range(range).inspect_err(|err| {
        log::error!("Response stream error: {}", err);
    });

    res.body(Body::wrap_stream(stream)).unwrap()
}

#[test]
fn test_base_hyper_response() {
    use {futures::Future, bytes::Bytes};
    let req = Request::builder()
        .body(Body::empty()).unwrap();

    let data = Bytes::from_static(b"0123456789");

    let res = hyper_response(&req, "application/test", "ETAG", "foo.zip", &data);

    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers().get(header::CONTENT_TYPE), Some(&header::HeaderValue::from_static("application/test")));
    assert_eq!(res.headers().get(header::CONTENT_DISPOSITION), Some(&header::HeaderValue::from_static("attachment; filename=\"foo.zip\"")));
    assert_eq!(res.headers().get(header::ETAG), Some(&header::HeaderValue::from_static("ETAG")));
    assert_eq!(res.headers().get(header::CONTENT_LENGTH), Some(&header::HeaderValue::from_static("10")));
    assert_eq!(res.into_body().concat2().wait().unwrap().as_ref(), b"0123456789");
}

#[test]
fn test_range_hyper_response() {
    use {futures::Future, bytes::Bytes};
    let req = Request::builder()
        .header(header::RANGE, "bytes=4-8")
        .header(header::IF_RANGE, "ETAG")
        .body(Body::empty()).unwrap();

    let data = Bytes::from_static(b"0123456789");

    let res = hyper_response(&req, "application/test", "ETAG", "foo.zip", &data);

    assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(res.headers().get(header::CONTENT_TYPE), Some(&header::HeaderValue::from_static("application/test")));
    assert_eq!(res.headers().get(header::ETAG), Some(&header::HeaderValue::from_static("ETAG")));
    assert_eq!(res.headers().get(header::CONTENT_LENGTH), Some(&header::HeaderValue::from_static("5")));
    assert_eq!(res.headers().get(header::CONTENT_RANGE), Some(&header::HeaderValue::from_static("bytes 4-8/10")));
    assert_eq!(res.into_body().concat2().wait().unwrap().as_ref(), b"45678");
}

#[test]
fn test_bad_if_range_hyper_response() {
    use {futures::Future, bytes::Bytes};
    let req = Request::builder()
        .header(header::RANGE, "bytes=4-8")
        .header(header::IF_RANGE, "WRONG")
        .body(Body::empty()).unwrap();

    let data = Bytes::from_static(b"0123456789");

    let res = hyper_response(&req, "application/test", "ETAG", "foo.zip", &data);

    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers().get(header::CONTENT_LENGTH), Some(&header::HeaderValue::from_static("10")));
    assert_eq!(res.headers().get(header::CONTENT_RANGE), None);
    assert_eq!(res.into_body().concat2().wait().unwrap().as_ref(), b"0123456789");
}
