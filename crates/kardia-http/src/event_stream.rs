//! Incremental parser for `text/event-stream` bodies.

use kardia_core::Result;

/// An event of a `text/event-stream` response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerSentEvent {
    /// The `event` field, or `message` when the event has none.
    pub event: String,
    /// The `data` fields, joined by line feeds.
    pub data: String,
    /// The last event id, which this or an earlier event set.
    pub id: String,
    /// The reconnection time in milliseconds, from the last valid `retry` field
    /// before the event.
    pub retry: Option<u64>,
}

/// Parses a `text/event-stream` body as the WHATWG HTML Standard describes it.
///
/// A chunk may end anywhere, including inside a line terminator or a UTF-8
/// sequence, so the parser keeps the partial line and the partial character for
/// the next call. An event is produced when the blank line that ends it arrives;
/// one still open at the end of the body is never produced.
#[derive(Default)]
pub struct EventStream {
    /// The bytes of a line that has not ended yet.
    pending: Vec<u8>,
    /// A UTF-8 sequence that a chunk ended in the middle of.
    partial: Vec<u8>,
    /// Whether the previous chunk ended with CR, so a leading LF belongs to it.
    skip_lf: bool,
    event: String,
    data: String,
    has_data: bool,
    id: String,
    retry: Option<u64>,
}

impl EventStream {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one chunk and appends the events it completes.
    pub fn push(&mut self, chunk: &[u8], events: &mut Vec<ServerSentEvent>) {
        // A character the previous chunk split is completed by this one.
        if !self.partial.is_empty() {
            let mut joined = std::mem::take(&mut self.partial);
            joined.extend_from_slice(chunk);
            let owned = joined.clone();
            self.push_complete(&owned, events);
            return;
        }
        self.push_complete(chunk, events);
    }

    fn push_complete(&mut self, chunk: &[u8], events: &mut Vec<ServerSentEvent>) {
        let mut start = 0;
        // The LF of a CRLF that the previous chunk split off belongs to that line.
        if self.skip_lf {
            self.skip_lf = false;
            if chunk.first() == Some(&b'\n') {
                start = 1;
            }
        }
        let mut index = start;
        while index < chunk.len() {
            let byte = chunk[index];
            let terminator = match byte {
                b'\r' => Some(if chunk.get(index + 1) == Some(&b'\n') { 2 } else { 1 }),
                b'\n' => Some(1),
                _ => None,
            };
            let Some(length) = terminator else {
                index += 1;
                continue;
            };
            // A CR that ends the chunk may be followed by the LF in the next one.
            if byte == b'\r' && index + 1 == chunk.len() {
                self.pending.extend_from_slice(&chunk[start..index]);
                self.line(events);
                self.skip_lf = true;
                return;
            }
            self.pending.extend_from_slice(&chunk[start..index]);
            self.line(events);
            index += length;
            start = index;
        }
        self.pending.extend_from_slice(&chunk[start..]);
        self.split_character();
    }

    /// Moves a trailing incomplete UTF-8 sequence out of `pending` into `partial`,
    /// so a chunk that ends inside a character does not replace it. The next chunk
    /// supplies the rest.
    fn split_character(&mut self) {
        let mut keep = 0;
        for length in 1..=4.min(self.pending.len()) {
            let tail = &self.pending[self.pending.len() - length..];
            let Some(first) = tail.first() else { continue };
            let expected = match first {
                0x00..=0x7f => 1,
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                // Not a leading byte: the sequence is malformed and is left for
                // the lossy conversion to report.
                _ => continue,
            };
            if expected > length && tail.iter().skip(1).all(|byte| byte & 0xc0 == 0x80) {
                keep = length;
            }
            break;
        }
        if keep > 0 {
            self.partial = self.pending.split_off(self.pending.len() - keep);
        }
    }

    fn line(&mut self, events: &mut Vec<ServerSentEvent>) {
        let line = std::mem::take(&mut self.pending);
        let line = String::from_utf8_lossy(&line).into_owned();
        if line.is_empty() {
            if self.has_data {
                events.push(ServerSentEvent {
                    event: if self.event.is_empty() { "message".to_owned() } else { std::mem::take(&mut self.event) },
                    data: std::mem::take(&mut self.data),
                    id: self.id.clone(),
                    retry: self.retry,
                });
            }
            self.event.clear();
            self.data.clear();
            self.has_data = false;
            return;
        }
        // A line that starts with a colon is a comment.
        if line.starts_with(':') {
            return;
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line.as_str(), ""),
        };
        match field {
            "event" => self.event = value.to_owned(),
            "data" => {
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.has_data = true;
            }
            "id" => {
                if !value.contains('\0') {
                    self.id = value.to_owned();
                }
            }
            "retry" if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) => {
                self.retry = value.parse().ok();
            }
            _ => {}
        }
    }

    /// Feeds a whole body, returning every event it completes.
    pub fn finish(body: &str) -> Vec<ServerSentEvent> {
        let mut parser = Self::new();
        let mut events = Vec::new();
        parser.push(body.as_bytes(), &mut events);
        events
    }
}

/// Parses a stream into its events, as `EventStream` describes them. Cancelling
/// the result cancels the body.
pub fn parse_event_stream(
    mut source: impl futures::Stream<Item = Result<bytes::Bytes>> + Send + Unpin + 'static,
) -> impl futures::Stream<Item = Result<ServerSentEvent>> + Send {
    use futures::StreamExt;
    let mut parser = EventStream::new();
    let mut queue = std::collections::VecDeque::new();
    futures::stream::poll_fn(move |cx| {
        loop {
            if let Some(event) = queue.pop_front() {
                return std::task::Poll::Ready(Some(Ok(event)));
            }
            match source.poll_next_unpin(cx) {
                std::task::Poll::Pending => return std::task::Poll::Pending,
                std::task::Poll::Ready(None) => return std::task::Poll::Ready(None),
                std::task::Poll::Ready(Some(Err(error))) => return std::task::Poll::Ready(Some(Err(error))),
                std::task::Poll::Ready(Some(Ok(chunk))) => {
                    let mut events = Vec::new();
                    parser.push(&chunk, &mut events);
                    queue.extend(events);
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{EventStream, ServerSentEvent};

    fn parse(body: &str) -> Vec<ServerSentEvent> {
        EventStream::finish(body)
    }

    #[test]
    fn reads_the_fields_of_an_event() {
        let events = parse("event: tick\ndata: 1\ndata: 2\nid: 7\nretry: 250\n\n");
        assert_eq!(
            events,
            vec![ServerSentEvent { event: "tick".into(), data: "1\n2".into(), id: "7".into(), retry: Some(250) }]
        );
    }

    #[test]
    fn an_event_without_a_name_is_a_message() {
        assert_eq!(parse("data: x\n\n")[0].event, "message");
    }

    #[test]
    fn a_comment_and_an_unknown_field_are_ignored() {
        // The comment would otherwise be the only content of the event.
        let events = parse(": keep-alive\ndata: x\nfoo: bar\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "x");
    }

    #[test]
    fn a_line_without_a_colon_is_a_field_with_an_empty_value() {
        let events = parse("data\n\n");
        assert_eq!(events[0].data, "");
        // `hasData` is set, so the event is produced with empty data.
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn one_leading_space_of_a_value_is_removed() {
        assert_eq!(parse("data:  two\n\n")[0].data, " two");
    }

    #[test]
    fn an_event_without_data_is_dropped() {
        assert!(parse("event: tick\n\n").is_empty());
        // A blank line with no fields at all does not produce one either.
        assert!(parse("\n\n").is_empty());
    }

    #[test]
    fn an_unterminated_event_at_the_end_is_dropped() {
        assert!(parse("data: x\n").is_empty());
    }

    #[test]
    fn an_id_with_a_nul_is_ignored() {
        let events = parse("id: a\0b\ndata: x\n\n");
        assert_eq!(events[0].id, "");
    }

    #[test]
    fn a_retry_that_is_not_a_number_is_ignored() {
        assert_eq!(parse("retry: 12x\ndata: x\n\n")[0].retry, None);
        assert_eq!(parse("retry:\ndata: x\n\n")[0].retry, None);
    }

    #[test]
    fn an_id_persists_to_the_next_event() {
        let events = parse("id: 9\ndata: a\n\ndata: b\n\n");
        assert_eq!(events[0].id, "9");
        assert_eq!(events[1].id, "9");
    }

    #[test]
    fn a_chunk_may_split_a_crlf_and_a_character() {
        let mut parser = EventStream::new();
        let mut events = Vec::new();
        // "数据" is split in the middle of its second character, and the CRLF
        // ending the data line is split between the two chunks.
        let body = "data: 数据\r\n\r\ndata: b\r\n\r\n";
        let bytes = body.as_bytes();
        for chunk in [&bytes[..8], &bytes[8..10], &bytes[10..]] {
            parser.push(chunk, &mut events);
        }
        assert_eq!(events.len(), 2, "{events:?}");
        assert_eq!(events[0].data, "数据", "the split character is restored");
        assert_eq!(events[1].data, "b");
    }

    #[test]
    fn a_lone_cr_ends_a_line() {
        assert_eq!(parse("data: a\rdata: b\r\r").len(), 1);
        assert_eq!(parse("data: a\rdata: b\r\r")[0].data, "a\nb");
    }
}
