//! Conservative evidence reduction, separate from format parsing and I/O.
use smartzip_archive::integrity::*;
use std::path::PathBuf;

fn unique(paths: impl Iterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut paths: Vec<_> = paths.collect();
    paths.sort();
    paths.dedup();
    paths
}

pub(crate) fn reduce(report: &mut TestArchiveReport) {
    report.missing_volumes.sort();
    report.missing_volumes.dedup();
    report.unreadable_volumes.sort();
    report.unreadable_volumes.dedup();
    report.confirmed_volumes.clear();
    report.suspect_groups.clear();
    let changed = report
        .evidence
        .iter()
        .any(|e| e.kind == EvidenceKind::InputChanged);
    let incomplete = !report.missing_volumes.is_empty()
        || !report.unreadable_volumes.is_empty()
        || report.volumes.ambiguous;
    for evidence in &report.evidence {
        let members = unique(
            evidence
                .ranges
                .iter()
                .chain(
                    if evidence.metadata_trust == MetadataTrust::ChecksumVerified {
                        [].iter()
                    } else {
                        evidence.reference_ranges.iter()
                    },
                )
                .map(|r| r.volume.clone()),
        );
        if evidence.strength == EvidenceStrength::Confirmed && !changed {
            // Confirmation always names one physical volume. No intersections
            // of overlapping suspect groups are used to fabricate certainty.
            if members.len() == 1 && !report.missing_volumes.contains(&members[0]) {
                if let Some(volume) = report
                    .confirmed_volumes
                    .iter_mut()
                    .find(|v| v.path == members[0])
                {
                    volume.evidence_ids.push(evidence.id.clone());
                    volume.ranges.extend(evidence.ranges.clone());
                } else {
                    report.confirmed_volumes.push(ConfirmedVolume {
                        path: members[0].clone(),
                        ranges: evidence.ranges.clone(),
                        evidence_ids: vec![evidence.id.clone()],
                    });
                }
            }
        } else if evidence.strength == EvidenceStrength::Suspect && !members.is_empty() && !changed
        {
            report.suspect_groups.push(SuspectGroup {
                members,
                relation: SuspectRelation::OneOrMore,
                evidence_ids: vec![evidence.id.clone()],
                affected_entries: evidence.affected_entries.clone(),
            });
        }
    }
    let last = report
        .passes
        .iter()
        .rev()
        .find(|p| p.purpose == "integrity")
        .or_else(|| report.passes.last());
    let backend_ok = last.is_some_and(|p| p.ok);
    let backend_failure = last.and_then(|p| p.diagnostics.failure);
    let local_damage = report.evidence.iter().any(|e| {
        e.strength == EvidenceStrength::Confirmed
            || e.strength == EvidenceStrength::Suspect
                && matches!(
                    e.kind,
                    EvidenceKind::HeaderChecksum
                        | EvidenceKind::PackedChecksum
                        | EvidenceKind::DataChecksum
                        | EvidenceKind::MetadataConflict
                        | EvidenceKind::StructuralTruncation
                )
    });
    let backend_damage = (matches!(backend_failure, Some(TestFailure::Corruption))
        || report.passes.iter().any(|p| {
            p.purpose == "diagnostic" && p.diagnostics.failure == Some(TestFailure::Corruption)
        }))
        && !incomplete;
    report.integrity = if changed {
        Integrity::Unknown
    } else if local_damage || backend_damage {
        Integrity::Corrupt
    } else if incomplete
        || matches!(
            backend_failure,
            Some(TestFailure::MissingVolume | TestFailure::Io)
        )
    {
        Integrity::Incomplete
    } else if backend_ok {
        Integrity::Intact
    } else {
        Integrity::Unknown
    };
    report.coverage = if report.integrity == Integrity::Intact {
        Coverage::Complete
    } else if !report.checked_scopes.is_empty()
        || last.is_some_and(|p| p.diagnostics.coverage != Coverage::None)
    {
        Coverage::Partial
    } else {
        Coverage::None
    };
    if report.integrity == Integrity::Intact {
        report.unchecked_volumes.clear();
    } else {
        report.unchecked_volumes = report.volumes.paths();
    }
    // An entry-level CRC with no usable metadata cannot narrow the group.
    // Keep an explicit possible group, including when a password prevents
    // deciding whether the data or credential is wrong.
    let unlocalized_files = report.damaged_files.iter().any(|file| {
        !report
            .evidence
            .iter()
            .any(|e| e.strength == EvidenceStrength::Confirmed && e.affected_entries.contains(file))
    });
    let unknown_open = report.integrity == Integrity::Unknown
        && backend_failure == Some(TestFailure::Unknown)
        && report.volumes.format.is_some();
    if !changed
        && report.integrity != Integrity::Intact
        && report.suspect_groups.is_empty()
        && (backend_damage && (report.confirmed_volumes.is_empty() || unlocalized_files)
            || matches!(backend_failure, Some(TestFailure::PasswordIndeterminate))
            || local_damage && report.confirmed_volumes.is_empty()
            || unknown_open)
    {
        let members = report.volumes.paths();
        if !members.is_empty() {
            report.suspect_groups.push(SuspectGroup {
                members,
                relation: SuspectRelation::Possible,
                evidence_ids: Vec::new(),
                affected_entries: report.damaged_files.clone(),
            });
        }
    }
    report.localization = if report.integrity == Integrity::Intact {
        Localization::NotApplicable
    } else if report.coverage == Coverage::Complete
        && !report.confirmed_volumes.is_empty()
        && report.suspect_groups.is_empty()
    {
        Localization::Exact
    } else if !report.confirmed_volumes.is_empty() || !report.suspect_groups.is_empty() {
        Localization::Partial
    } else {
        Localization::Unknown
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use smartzip_archive::volumes::{VolumeFamily, VolumeSet};

    #[test]
    fn overlapping_suspect_groups_never_intersect_into_confirmation() {
        let mut report = TestArchiveReport::new(
            VolumeSet {
                family: VolumeFamily::RarPart,
                format: None,
                entrypoint: "a.part1.rar".into(),
                members: Vec::new(),
                missing: Vec::new(),
                unreadable: Vec::new(),
                issues: Vec::new(),
                ambiguous: false,
            },
            "a.part1.rar".into(),
        );
        for (i, numbers) in [[2, 3], [3, 4]].iter().enumerate() {
            report.evidence.push(TestEvidence {
                id: i.to_string(),
                kind: EvidenceKind::PackedChecksum,
                strength: EvidenceStrength::Suspect,
                source: "fixture".into(),
                pass_id: 1,
                ranges: numbers
                    .iter()
                    .map(|n| PhysicalRange {
                        volume: format!("part{n}").into(),
                        offset: 0,
                        length: 10,
                    })
                    .collect(),
                reference_ranges: Vec::new(),
                metadata_trust: MetadataTrust::ChecksumVerified,
                affected_entries: Vec::new(),
                summary: String::new(),
            });
        }
        reduce(&mut report);
        assert!(report.confirmed_volumes.is_empty());
        assert_eq!(report.suspect_groups.len(), 2);
        assert_eq!(
            report.suspect_groups[0].members,
            vec![PathBuf::from("part2"), PathBuf::from("part3")]
        );
    }
}
