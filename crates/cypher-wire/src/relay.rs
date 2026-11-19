//! Frames on the onion relay: `[corr u64][tag u8][payload]` from the client,
//! `[corr u64][reply]` back. The correlation id is the client's, echoed so
//! pipelined requests can be told apart.

pub const CORR_LEN: usize = 8;
/// What follows the tag.
pub const HEADER_LEN: usize = CORR_LEN + 1;

/// A request sealed to signaling's onion key (`[epoch u32][sealed]`).
pub const TAG_ONION: u8 = 1;
/// Signaling's signed onion key set; no payload.
pub const TAG_KEYS: u8 = 2;

/// Where a parsed request goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Onion,
    Keys,
}

/// A validated relay request, borrowing its opaque payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Request<'a> {
    pub corr: u64,
    pub route: Route,
    pub payload: &'a [u8],
}

/// Parses a complete request, rejecting short frames, unknown routes and a
/// payload on the key-set route.
pub fn parse(frame: &[u8]) -> Option<Request<'_>> {
    let (corr, rest) = frame.split_first_chunk::<CORR_LEN>()?;
    let (tag, payload) = rest.split_first()?;
    let route = match *tag {
        TAG_ONION => Route::Onion,
        TAG_KEYS if payload.is_empty() => Route::Keys,
        _ => return None,
    };
    Some(Request {
        corr: u64::from_le_bytes(*corr),
        route,
        payload,
    })
}

/// Splits a relay reply into its correlation id and opaque response body.
pub fn parse_reply(reply: &[u8]) -> Option<(u64, &[u8])> {
    let (corr, body) = reply.split_first_chunk::<CORR_LEN>()?;
    Some((u64::from_le_bytes(*corr), body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_routes_and_boundaries() {
        let onion = [1u64.to_le_bytes().as_slice(), &[TAG_ONION], &[7, 8]].concat();
        assert_eq!(
            parse(&onion),
            Some(Request {
                corr: 1,
                route: Route::Onion,
                payload: &[7, 8],
            })
        );
        let keys = [2u64.to_le_bytes().as_slice(), &[TAG_KEYS]].concat();
        assert_eq!(
            parse(&keys),
            Some(Request {
                corr: 2,
                route: Route::Keys,
                payload: &[],
            })
        );
        for len in 0..HEADER_LEN {
            assert_eq!(parse(&vec![TAG_ONION; len]), None, "length {len}");
        }
        assert_eq!(parse(&[keys.as_slice(), &[0]].concat()), None);
        let unknown = [0u64.to_le_bytes().as_slice(), &[0xff]].concat();
        assert_eq!(parse(&unknown), None);
    }

    #[test]
    fn reply_requires_only_the_correlation_prefix() {
        assert_eq!(parse_reply(&[0; CORR_LEN - 1]), None);
        let reply = [0x0807_0605_0403_0201u64.to_le_bytes().as_slice(), &[9]].concat();
        assert_eq!(parse_reply(&reply), Some((0x0807_0605_0403_0201, &[9][..])));
        assert_eq!(parse_reply(&[0; CORR_LEN]), Some((0, &[][..])));
    }
}
