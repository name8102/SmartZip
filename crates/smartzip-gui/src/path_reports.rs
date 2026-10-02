//! Presentation of complete reports; event summaries are never used to reconstruct names.
use smartzip_core::{PathMappingReason, PathMappingReport};

pub const PAGE_SIZE: usize = 50;

pub fn reports_from_result(result: Option<&serde_json::Value>) -> Vec<PathMappingReport> {
    result
        .and_then(|value| value.get("path_reports"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default()
}

pub fn reports_for_file(
    result: Option<&serde_json::Value>,
    path: &std::path::Path,
    node_id: &str,
) -> Vec<PathMappingReport> {
    let all = reports_from_result(result);
    let path = path.to_string_lossy();
    let matching = all
        .iter()
        .filter(|report| match report.node_id.as_deref() {
            Some(id) => id == node_id,
            None => report.archive_path.as_str() == path.as_ref(),
        })
        .cloned()
        .collect::<Vec<_>>();
    if matching.is_empty()
        && all
            .iter()
            .all(|report| report.node_id.is_none() && report.archive_path.is_empty())
    {
        all
    } else {
        matching
    }
}

pub fn reason_label(reason: PathMappingReason) -> &'static str {
    match reason {
        PathMappingReason::InvalidCharacters => "替换非法字符",
        PathMappingReason::TrailingCharacters => "清理尾部空格或句点",
        PathMappingReason::ReservedName => "系统保留名称",
        PathMappingReason::NameTooLong => "名称超过分量预算",
        PathMappingReason::NameCollision => "目标名称等价冲突",
        PathMappingReason::ExtensionShortened => "缩短扩展名",
        PathMappingReason::ChangedAncestor => "上级目录已调整",
        PathMappingReason::CreationFallback => "实际创建后缩短回退",
        PathMappingReason::LayoutRenamed => "最终布局名称已调整",
        PathMappingReason::FullPathShortened => "完整路径过长，已缩短目录或文件名",
    }
}

pub fn failure_label(reason: &str) -> &str {
    match reason {
        "name_too_long" => "单个名称过长",
        "path_too_long" => "完整路径超出当前支持范围",
        "invalid_name" => "目标文件系统拒绝名称",
        "name_collision" => "归档名称冲突",
        "path_remap_unsupported" => "当前后端无法安全映射名称",
        "path_constraint_unknown" => "无法确定路径限制原因",
        _ => reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(path: &str, node: &str) -> serde_json::Value {
        serde_json::json!({
            "version":1,"digest":node,"tentative":false,"archive_path":path,"node_id":node,
            "entries":[],
            "policy":{
                "version":1,"mode":"native","fs_kind":"test",
                "target":{"canonical_root":"/out","volume_id":"test","root_id":"root"},
                "component":{"limit":255,"metric":"utf8_bytes","source":"test","confidence":"known"},
                "access":"posix_dir_relative",
                "comparison":{"case_sensitive":true,"normalization_sensitive":true,"confidence":"known"},
                "windows_names":false,"full_path_limit":null
            }
        })
    }

    #[test]
    fn current_file_details_use_complete_reports_for_the_matching_node() {
        let result = serde_json::json!({"path_reports":[report("/first.zip","first"),report("/second.zip","second")]});
        let matching =
            reports_for_file(Some(&result), std::path::Path::new("/second.zip"), "second");
        assert_eq!(matching.len(), 1);
        assert_eq!(matching[0].node_id.as_deref(), Some("second"));
        assert_eq!(
            reports_for_file(
                Some(&result),
                std::path::Path::new("/legacy.zip"),
                "unknown"
            )
            .len(),
            0
        );
        let legacy = serde_json::json!({"path_reports":[report("", "")]});
        let mut legacy = legacy;
        legacy["path_reports"][0]["node_id"] = serde_json::Value::Null;
        assert_eq!(
            reports_for_file(
                Some(&legacy),
                std::path::Path::new("/legacy.zip"),
                "unknown"
            )
            .len(),
            1
        );
    }
}
