//! GUI submission grouping uses the same resolver as extraction.
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

#[derive(Clone, Debug, PartialEq)]
pub struct VolumeGroupDetails {
    pub inputs: Vec<PathBuf>,
    pub members: Vec<PathBuf>,
    pub candidates: Vec<Vec<PathBuf>>,
    pub selected: Vec<Vec<PathBuf>>,
    pub diagnostic: String,
}

/// Independent groups may run concurrently. Overlapping hypotheses form one
/// control group, preserving every seed until the workflow resolves the winner.
pub fn group_root_inputs(inputs: &[PathBuf], discover: bool) -> Vec<VolumeGroupDetails> {
    let mut resolver = crate::volumes::VolumeResolver::new();
    let seeds = inputs
        .iter()
        .map(|input| {
            let resolution = if discover {
                resolver.resolve(&crate::ExtractionCandidate::root(input.clone()))
            } else {
                crate::volumes::VolumeResolution::Single
            };
            use crate::volumes::VolumeResolution::*;
            match resolution {
                Resolved(set) | ResolvedWithWarnings { set, .. } => {
                    let members: Vec<_> = set.members.into_iter().map(|m| m.path).collect();
                    VolumeGroupDetails {
                        inputs: vec![set.entrypoint],
                        candidates: vec![members.clone()],
                        members,
                        selected: vec![],
                        diagnostic: "已识别分卷组，等待执行确认".into(),
                    }
                }
                GroupingAmbiguous { hypotheses } => {
                    let candidates: Vec<Vec<_>> = hypotheses
                        .into_iter()
                        .map(|h| h.members.into_iter().map(|m| m.path).collect())
                        .collect();
                    let mut members: Vec<_> = candidates.iter().flatten().cloned().collect();
                    members.push(input.clone());
                    members.sort();
                    members.dedup();
                    VolumeGroupDetails {
                        inputs: vec![input.clone()],
                        members,
                        candidates,
                        selected: vec![],
                        diagnostic: "分组待判定：候选组可能共享成员，将依次验证".into(),
                    }
                }
                Incomplete(problem) => VolumeGroupDetails {
                    inputs: vec![input.clone()],
                    members: vec![input.clone()],
                    candidates: vec![],
                    selected: vec![],
                    diagnostic: format!("分卷不完整：{}", problem.reason),
                },
                Single => VolumeGroupDetails {
                    inputs: vec![input.clone()],
                    members: vec![input.clone()],
                    candidates: vec![],
                    selected: vec![],
                    diagnostic: String::new(),
                },
            }
        })
        .collect();
    merge_overlapping(seeds)
}
fn merge_overlapping(seeds: Vec<VolumeGroupDetails>) -> Vec<VolumeGroupDetails> {
    let mut parents: Vec<_> = (0..seeds.len()).collect();
    fn root(parents: &mut [usize], mut i: usize) -> usize {
        while parents[i] != i {
            parents[i] = parents[parents[i]];
            i = parents[i];
        }
        i
    }
    let mut owners = HashMap::new();
    for (i, seed) in seeds.iter().enumerate() {
        for path in seed.members.iter().chain(&seed.inputs) {
            if let Some(previous) = owners.insert(path.clone(), i) {
                let a = root(&mut parents, i);
                let b = root(&mut parents, previous);
                parents[a.max(b)] = a.min(b);
            }
        }
    }
    let mut groups: Vec<VolumeGroupDetails> = Vec::new();
    let mut indices = HashMap::new();
    for (i, seed) in seeds.into_iter().enumerate() {
        let owner = root(&mut parents, i);
        if let Some(&index) = indices.get(&owner) {
            let group: &mut VolumeGroupDetails = &mut groups[index];
            group.inputs.extend(seed.inputs);
            group.members.extend(seed.members);
            group.candidates.extend(seed.candidates);
            if !seed.diagnostic.is_empty() && !group.diagnostic.contains(&seed.diagnostic) {
                group.diagnostic.push('；');
                group.diagnostic.push_str(&seed.diagnostic);
            }
        } else {
            indices.insert(owner, groups.len());
            groups.push(seed);
        }
    }
    for group in &mut groups {
        let mut seen = HashSet::new();
        group.inputs.retain(|p| seen.insert(p.clone()));
        group.members.sort();
        group.members.dedup();
        for candidate in &mut group.candidates {
            candidate.sort();
            candidate.dedup();
        }
        group.candidates.sort();
        group.candidates.dedup();
        if group.candidates.len() > 1 {
            group.diagnostic = "分组待判定：重叠候选作为一个任务控制，保留所有输入逐一验证".into();
        }
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overlapping_hypotheses_merge_transitively_without_losing_seeds() {
        let seed = |input: &str, members: &[&str]| VolumeGroupDetails {
            inputs: vec![input.into()],
            members: members.iter().map(PathBuf::from).collect(),
            candidates: vec![members.iter().map(PathBuf::from).collect()],
            selected: vec![],
            diagnostic: String::new(),
        };
        let groups = merge_overlapping(vec![
            seed("a", &["a", "b"]),
            seed("c", &["b", "c"]),
            seed("d", &["d", "e"]),
            seed("f", &["c", "f"]),
        ]);
        assert_eq!(groups.len(), 2);
        assert_eq!(
            groups[0].inputs,
            vec![PathBuf::from("a"), "c".into(), "f".into()]
        );
        assert_eq!(groups[0].candidates.len(), 3);
        assert_eq!(groups[1].inputs, vec![PathBuf::from("d")]);
    }
}
