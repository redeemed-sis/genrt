use std::collections::HashMap;

use anyhow::{Result, bail};

const RECORD_SEPARATOR: u8 = 0x1e;
const MAX_RECORD_BYTES: usize = 512;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Event {
    Ready,
    CaseStart,
    Pass,
    Fail,
    Done,
    Abort,
}

impl Event {
    pub(super) fn parse(value: &str) -> Result<Self> {
        match value {
            "READY" => Ok(Self::Ready),
            "CASE_START" => Ok(Self::CaseStart),
            "PASS" => Ok(Self::Pass),
            "FAIL" => Ok(Self::Fail),
            "DONE" => Ok(Self::Done),
            "ABORT" => Ok(Self::Abort),
            _ => bail!("unknown GTRT/1 event {value:?}"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Record {
    pub(super) producer: String,
    pub(super) sequence: usize,
    pub(super) event: Event,
    pub(super) subject: String,
    pub(super) detail: Option<String>,
}

#[derive(Default)]
pub(super) struct StreamParser {
    record: Vec<u8>,
    collecting: bool,
}

impl StreamParser {
    pub(super) fn push(&mut self, input: &[u8]) -> Vec<Result<Record>> {
        let mut records = Vec::new();
        for &byte in input {
            if byte == RECORD_SEPARATOR {
                if self.collecting && !self.record.is_empty() {
                    records.push(Err(anyhow::anyhow!("nested record separator")));
                }
                self.record.clear();
                self.collecting = true;
                continue;
            }
            if !self.collecting {
                continue;
            }
            if byte == b'\n' {
                if self.record.last() == Some(&b'\r') {
                    self.record.pop();
                }
                records.push(parse_record(&self.record));
                self.record.clear();
                self.collecting = false;
                continue;
            }
            if self.record.len() == MAX_RECORD_BYTES {
                records.push(Err(anyhow::anyhow!("GTRT/1 record exceeds byte limit")));
                self.record.clear();
                self.collecting = false;
                continue;
            }
            self.record.push(byte);
        }
        records
    }

    pub(super) fn finish(&self) -> Result<()> {
        if self.collecting {
            bail!("incomplete GTRT/1 record at end of stream");
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CaseLifecycle {
    Started,
    Passed,
}

pub(super) struct ProtocolState {
    supervisor: String,
    suite: String,
    next_sequence: HashMap<String, usize>,
    cases: HashMap<(String, String), CaseLifecycle>,
    ready: bool,
    terminal: bool,
}

impl ProtocolState {
    pub(super) fn new(supervisor: &str, suite: &str) -> Self {
        Self {
            supervisor: supervisor.to_owned(),
            suite: suite.to_owned(),
            next_sequence: HashMap::new(),
            cases: HashMap::new(),
            ready: false,
            terminal: false,
        }
    }

    pub(super) fn observe(&mut self, record: &Record) -> Result<()> {
        if self.terminal {
            bail!("protocol record observed after terminal DONE");
        }
        let next = self
            .next_sequence
            .entry(record.producer.clone())
            .or_insert(1);
        if record.sequence != *next {
            bail!(
                "producer {:?} sequence {}, expected {}",
                record.producer,
                record.sequence,
                *next
            );
        }
        *next += 1;

        match record.event {
            Event::Ready => {
                if record.producer != self.supervisor
                    || record.subject != self.suite
                    || record.detail.is_some()
                    || self.ready
                {
                    bail!("invalid or duplicate READY record: {record:?}");
                }
                self.ready = true;
            }
            Event::Done => {
                if !self.ready
                    || record.producer != self.supervisor
                    || record.subject != self.suite
                    || record.detail.as_deref() != Some("PASS")
                {
                    bail!("invalid terminal DONE record: {record:?}");
                }
                if self
                    .cases
                    .values()
                    .any(|state| *state != CaseLifecycle::Passed)
                {
                    bail!("terminal DONE observed with unfinished cases");
                }
                self.terminal = true;
            }
            Event::Fail | Event::Abort => {
                bail!("guest reported {:?}: {record:?}", record.event);
            }
            Event::CaseStart | Event::Pass if !self.ready => {
                bail!("case record observed before READY: {record:?}");
            }
            Event::CaseStart => {
                if record.detail.is_some() {
                    bail!("CASE_START must not contain detail: {record:?}");
                }
                let key = (record.producer.clone(), record.subject.clone());
                if self.cases.insert(key, CaseLifecycle::Started).is_some() {
                    bail!("duplicate CASE_START record: {record:?}");
                }
            }
            Event::Pass => {
                let key = (record.producer.clone(), record.subject.clone());
                match self.cases.get_mut(&key) {
                    Some(state @ CaseLifecycle::Started) => *state = CaseLifecycle::Passed,
                    Some(CaseLifecycle::Passed) => bail!("duplicate PASS record: {record:?}"),
                    None => bail!("PASS observed without CASE_START: {record:?}"),
                }
            }
        }
        Ok(())
    }

    pub(super) fn terminal(&self) -> bool {
        self.terminal
    }

    pub(super) fn validate_complete(&self) -> Result<()> {
        if !self.ready {
            bail!("protocol stream has no READY record");
        }
        if !self.terminal {
            bail!("protocol stream has no terminal DONE PASS record");
        }
        Ok(())
    }
}

fn parse_record(bytes: &[u8]) -> Result<Record> {
    let text = std::str::from_utf8(bytes)?;
    let fields = text.split('|').collect::<Vec<_>>();
    if !(fields.len() == 5 || fields.len() == 6) {
        bail!("GTRT/1 record has {} fields", fields.len());
    }
    if fields[0] != "GTRT/1" {
        bail!("unsupported test protocol version {:?}", fields[0]);
    }
    for field in &fields[1..] {
        if field.is_empty()
            || !field
                .bytes()
                .all(|byte| byte.is_ascii_graphic() && byte != b'|')
        {
            bail!("invalid GTRT/1 field {field:?}");
        }
    }
    if fields[2].len() != 6 || !fields[2].bytes().all(|byte| byte.is_ascii_digit()) {
        bail!("invalid GTRT/1 sequence {:?}", fields[2]);
    }
    let sequence = fields[2].parse::<usize>()?;
    if sequence == 0 {
        bail!("GTRT/1 sequence starts at one");
    }
    Ok(Record {
        producer: fields[1].to_owned(),
        sequence,
        event: Event::parse(fields[3])?,
        subject: fields[4].to_owned(),
        detail: fields.get(5).map(|value| (*value).to_owned()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire(value: &str) -> Vec<u8> {
        let mut out = vec![RECORD_SEPARATOR];
        out.extend_from_slice(value.as_bytes());
        out.push(b'\n');
        out
    }

    #[test]
    fn parses_records_split_across_chunks_and_ignores_logs() {
        let mut parser = StreamParser::default();
        assert!(parser.push(b"human log\n\x1eGTRT/1|sup|000").is_empty());
        let records = parser.push(b"001|READY|suite\r\nmore logs");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].as_ref().unwrap().event, Event::Ready);
    }

    #[test]
    fn rejects_unknown_versions_and_sequence_gaps() {
        let mut parser = StreamParser::default();
        assert!(parser.push(&wire("GTRT/2|sup|000001|READY|suite"))[0].is_err());
        let mut state = ProtocolState::new("sup", "suite");
        let record = parse_record(b"GTRT/1|sup|000002|READY|suite").unwrap();
        assert!(state.observe(&record).is_err());
    }

    #[test]
    fn rejects_duplicate_terminal_and_failures() {
        let mut state = ProtocolState::new("sup", "suite");
        state
            .observe(&parse_record(b"GTRT/1|sup|000001|READY|suite").unwrap())
            .unwrap();
        state
            .observe(&parse_record(b"GTRT/1|sup|000002|CASE_START|case").unwrap())
            .unwrap();
        state
            .observe(&parse_record(b"GTRT/1|sup|000003|PASS|case").unwrap())
            .unwrap();
        state
            .observe(&parse_record(b"GTRT/1|sup|000004|DONE|suite|PASS").unwrap())
            .unwrap();
        assert!(
            state
                .observe(&parse_record(b"GTRT/1|sup|000005|DONE|suite|PASS").unwrap())
                .is_err()
        );

        let mut state = ProtocolState::new("sup", "suite");
        state
            .observe(&parse_record(b"GTRT/1|sup|000001|READY|suite").unwrap())
            .unwrap();
        assert!(
            state
                .observe(&parse_record(b"GTRT/1|sup|000002|FAIL|case|BAD").unwrap())
                .is_err()
        );
    }

    #[test]
    fn enforces_case_lifecycle_and_complete_tail() {
        let mut state = ProtocolState::new("sup", "suite");
        state
            .observe(&parse_record(b"GTRT/1|sup|000001|READY|suite").unwrap())
            .unwrap();
        assert!(
            state
                .observe(&parse_record(b"GTRT/1|worker|000001|PASS|case").unwrap())
                .is_err()
        );

        let mut parser = StreamParser::default();
        assert!(parser.push(b"\x1eGTRT/1|sup|000001").is_empty());
        assert!(parser.finish().is_err());
    }
}
