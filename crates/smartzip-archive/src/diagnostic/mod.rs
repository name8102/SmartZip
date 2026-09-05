//! Bounded read-only format diagnostics. No recovery writes or dummy volumes.
mod rar;
mod sevenz;
mod zip;

use crate::integrity::*;
use crate::volumes::VolumeSet;
use smartzip_core::ArchiveFormat;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Instant;

pub(super) const MAX_METADATA: usize = 16 * 1024 * 1024;
pub(super) const MAX_RECORDS: usize = 100_000;
const MAX_EVIDENCE: usize = 4096;

#[derive(Clone, Default, Debug)]
pub struct DiagnosticControl {
    cancelled: Arc<AtomicBool>,
    pub deadline: Option<Instant>,
}

impl DiagnosticControl {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
    pub fn check(&self) -> io::Result<()> {
        if self.is_cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        if self.deadline.is_some_and(|limit| Instant::now() >= limit) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "diagnostic timeout reached",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct DiagnosticReport {
    pub evidence: Vec<TestEvidence>,
    pub checked_scopes: Vec<CheckedScope>,
    pub missing: Vec<PathBuf>,
    pub unreadable: Vec<PathBuf>,
    pub stop_reasons: Vec<String>,
    pub encrypted: Option<bool>,
}

impl DiagnosticReport {
    fn evidence(&mut self, mut evidence: TestEvidence) {
        if self.evidence.len() >= MAX_EVIDENCE {
            self.stop("diagnostic evidence budget reached");
            return;
        }
        evidence.id = format!("local-{}", self.evidence.len() + 1);
        self.evidence.push(evidence);
    }
    fn stop(&mut self, reason: impl Into<String>) {
        let reason = reason.into();
        if !self.stop_reasons.contains(&reason) && self.stop_reasons.len() < MAX_EVIDENCE {
            self.stop_reasons.push(reason);
        }
    }
    fn checked(&mut self, scope: CheckedScope) {
        if self.checked_scopes.len() < MAX_EVIDENCE {
            self.checked_scopes.push(scope);
        }
    }
}

pub fn inspect(
    set: &VolumeSet,
    failed_files: &[String],
    control: &DiagnosticControl,
    pass_id: u32,
) -> DiagnosticReport {
    let mut report = DiagnosticReport::default();
    let result = match set.format {
        Some(ArchiveFormat::Rar) => rar::inspect(set, control, pass_id, &mut report),
        Some(ArchiveFormat::SevenZip) => {
            sevenz::inspect(set, failed_files, control, pass_id, &mut report)
        }
        Some(ArchiveFormat::Zip) => zip::inspect(set, failed_files, control, pass_id, &mut report),
        _ => {
            report.stop("no independent format diagnostic is available");
            Ok(())
        }
    };
    if let Err(error) = result {
        report.stop(error.to_string());
    }
    report
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn read_at(reader: &mut (impl Read + Seek), offset: u64, length: usize) -> io::Result<Vec<u8>> {
    if length > MAX_METADATA {
        return Err(invalid("metadata size budget exceeded"));
    }
    reader.seek(SeekFrom::Start(offset))?;
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn crc_range(
    reader: &mut (impl Read + Seek),
    offset: u64,
    size: u64,
    control: &DiagnosticControl,
) -> io::Result<u32> {
    reader.seek(SeekFrom::Start(offset))?;
    let mut left = size;
    let mut buffer = [0; 64 * 1024];
    let mut crc = crc32fast::Hasher::new();
    while left > 0 {
        control.check()?;
        let count = left.min(buffer.len() as u64) as usize;
        reader.read_exact(&mut buffer[..count])?;
        crc.update(&buffer[..count]);
        left -= count as u64;
    }
    Ok(crc.finalize())
}

fn whole_set(set: &VolumeSet) -> Vec<PhysicalRange> {
    set.members
        .iter()
        .map(|m| PhysicalRange {
            volume: m.path.clone(),
            offset: 0,
            length: m.size,
        })
        .collect()
}

fn paths(ranges: &[PhysicalRange]) -> Vec<PathBuf> {
    let mut paths: Vec<_> = ranges.iter().map(|r| r.volume.clone()).collect();
    paths.sort();
    paths.dedup();
    paths
}

struct Bytes<'a> {
    data: &'a [u8],
    pos: usize,
}
impl<'a> Bytes<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }
    fn take(&mut self, size: usize) -> io::Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(size)
            .ok_or_else(|| invalid("metadata offset overflow"))?;
        let bytes = self
            .data
            .get(self.pos..end)
            .ok_or_else(|| invalid("truncated metadata"))?;
        self.pos = end;
        Ok(bytes)
    }
    fn byte(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> io::Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> io::Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self) -> io::Result<u64> {
        let lo = u64::from(self.u32()?);
        let hi = u64::from(self.u32()?);
        Ok(lo | hi << 32)
    }
    fn vint(&mut self) -> io::Result<u64> {
        let mut value = 0;
        for shift in (0..70).step_by(7) {
            let byte = self.byte()?;
            if shift == 63 && byte > 1 {
                return Err(invalid("RAR integer overflow"));
            }
            value |= u64::from(byte & 127) << shift;
            if byte & 128 == 0 {
                return Ok(value);
            }
        }
        Err(invalid("invalid RAR integer"))
    }
    fn seven(&mut self) -> io::Result<u64> {
        let first = self.byte()?;
        let mut mask = 0x80;
        let mut value = 0;
        for index in 0..8 {
            if first & mask == 0 {
                return Ok(value | u64::from(first & (mask - 1)) << (index * 8));
            }
            value |= u64::from(self.byte()?) << (index * 8);
            mask >>= 1;
        }
        Ok(value)
    }
    fn count(&mut self) -> io::Result<usize> {
        let count = self.seven()?;
        if count > MAX_RECORDS as u64 {
            return Err(invalid("metadata record budget exceeded"));
        }
        Ok(count as usize)
    }
}
