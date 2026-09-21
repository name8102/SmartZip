//! Presentation metadata only. Values and defaults belong to `smartzip-config`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Bool,
    Choice(&'static [(&'static str, &'static str)]),
    Text,
    Number,
    Toml,
    ReadOnly,
}

#[derive(Debug, Clone, Copy)]
pub struct Field {
    pub key: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub kind: Kind,
}

#[derive(Debug, Clone, Copy)]
pub struct Section {
    pub label: &'static str,
    pub fields: &'static [Field],
}

macro_rules! field {
    ($key:literal, $label:literal, $help:literal, $kind:expr) => {
        Field {
            key: $key,
            label: $label,
            help: $help,
            kind: $kind,
        }
    };
}

const STATE_MODES: &[(&str, &str)] = &[
    ("off", "关闭"),
    ("read-only", "只读"),
    ("read-write", "读写"),
];

pub const SECTIONS: &[Section] = &[
    Section {
        label: "解压与输出",
        fields: &[
            field!(
                "extraction.on_error",
                "出错时",
                "当前输入失败后是否继续处理其他输入。",
                Kind::Choice(&[("continue", "继续"), ("stop", "停止")])
            ),
            field!(
                "extraction.recursion.enabled",
                "递归解压",
                "继续解压输出目录中的嵌套归档。",
                Kind::Bool
            ),
            field!(
                "extraction.recursion.max_depth",
                "最大递归深度",
                "0 表示不继续递归；取值范围为 0–255。",
                Kind::Number
            ),
            field!(
                "extraction.output.destination",
                "输出位置",
                "选择指定目录时，必须同时填写输出目录。",
                Kind::Choice(&[
                    ("first-input-parent", "首个输入所在目录"),
                    ("directory", "指定目录")
                ])
            ),
            field!(
                "extraction.output.directory",
                "输出目录",
                "可选路径；选择指定目录时必填。",
                Kind::Text
            ),
            field!(
                "extraction.output.layout",
                "输出布局",
                "控制目录包装与文件平铺方式。",
                Kind::Choice(&[
                    ("conservative", "保守"),
                    ("smart", "智能"),
                    ("raw", "原始"),
                    ("flat-single", "单文件平铺")
                ])
            ),
            field!(
                "extraction.output.single_root_name",
                "单根目录命名",
                "归档只有一个顶层目录时的命名策略。",
                Kind::Choice(&[
                    ("auto", "自动"),
                    ("archive", "归档名称"),
                    ("inner", "内部目录名称"),
                    ("preserve-both", "保留两者")
                ])
            ),
            field!(
                "extraction.output.on_conflict",
                "文件冲突",
                "输出文件重名时的处理方式。",
                Kind::Choice(&[
                    ("ask", "询问"),
                    ("skip", "跳过"),
                    ("rename", "重命名"),
                    ("overwrite", "覆盖")
                ])
            ),
            field!(
                "extraction.cleanup.nested_archives",
                "嵌套归档清理",
                "仅对成功解压的嵌套归档生效；不会删除用户投入的顶层归档。永久删除不可恢复。",
                Kind::Choice(&[
                    ("keep", "保留"),
                    ("trash", "移入废纸篓"),
                    ("delete", "永久删除")
                ])
            ),
        ],
    },
    Section {
        label: "扫描与编码",
        fields: &[
            field!(
                "extraction.embedded.root",
                "根输入嵌入扫描",
                "扫描输入文件中嵌入的归档候选。",
                Kind::Choice(&[
                    ("off", "关闭"),
                    ("auto", "自动"),
                    ("ask", "询问"),
                    ("all", "全部"),
                    ("largest", "最大候选")
                ])
            ),
            field!(
                "extraction.embedded.nested",
                "嵌套嵌入扫描",
                "控制递归过程的嵌入扫描；递归关闭时不生效。",
                Kind::Choice(&[
                    ("off", "关闭"),
                    ("auto", "自动"),
                    ("ask", "询问"),
                    ("aggressive", "积极"),
                    ("all", "全部"),
                    ("largest", "最大候选")
                ])
            ),
            field!(
                "extraction.embedded.dominant_min_ratio",
                "主候选占比阈值",
                "范围为 0–1，支持小数。",
                Kind::Number
            ),
            field!(
                "extraction.volumes.auto_discover",
                "自动发现分卷",
                "查找相邻的分卷归档文件。",
                Kind::Bool
            ),
            field!(
                "extraction.encoding.mode",
                "文件名编码",
                "自动检测、交给后端或指定编码。",
                Kind::Choice(&[
                    ("auto", "自动"),
                    ("backend", "后端默认"),
                    ("utf-8", "UTF-8"),
                    ("gb18030", "GB18030"),
                    ("gbk", "GBK"),
                    ("big5", "Big5"),
                    ("shift_jis", "Shift JIS"),
                    ("euc-jp", "EUC-JP"),
                    ("euc-kr", "EUC-KR")
                ])
            ),
            field!(
                "extraction.encoding.on_suspicious",
                "可疑编码",
                "检测到可疑文件名编码时的处理方式。",
                Kind::Choice(&[("ask", "询问"), ("skip", "跳过"), ("accept", "接受")])
            ),
        ],
    },
    Section {
        label: "密码策略",
        fields: &[
            field!(
                "passwords.mode",
                "密码模式",
                "此处配置策略；密码明文应通过任务输入或密码管理处理，不写入配置。",
                Kind::Choice(&[("auto", "自动"), ("manual", "手动"), ("off", "关闭")])
            ),
            field!(
                "passwords.sources",
                "候选来源顺序",
                "TOML 字符串数组，按顺序使用 manual、known、batch、empty、database；不可重复。不是密码值列表。",
                Kind::Toml
            ),
            field!(
                "passwords.database_limit",
                "密码库候选上限",
                "从密码库选取的候选数量上限。",
                Kind::Number
            ),
            field!(
                "passwords.save_success",
                "保存成功密码",
                "需要状态数据库处于读写模式。",
                Kind::Bool
            ),
            field!(
                "passwords.record_statistics",
                "记录密码统计",
                "需要状态数据库处于读写模式。",
                Kind::Bool
            ),
        ],
    },
    Section {
        label: "历史与状态",
        fields: &[
            field!(
                "state.database",
                "状态数据库路径",
                "可选路径；留空使用应用默认路径。",
                Kind::Text
            ),
            field!(
                "state.mode",
                "数据库模式",
                "只读或关闭时会限制历史、密码及已知文件信息写入。",
                Kind::Choice(STATE_MODES)
            ),
            field!(
                "state.history",
                "记录任务历史",
                "需要状态数据库处于读写模式。",
                Kind::Bool
            ),
            field!(
                "state.known_files",
                "已知文件记录",
                "实际读写能力受数据库模式限制。",
                Kind::Choice(STATE_MODES)
            ),
            field!(
                "extraction.reuse.password_hint",
                "复用密码提示",
                "允许使用已有文件记录中的密码提示。",
                Kind::Bool
            ),
            field!(
                "extraction.reuse.encoding_hint",
                "复用编码提示",
                "允许使用已有文件记录中的编码提示。",
                Kind::Bool
            ),
            field!(
                "extraction.reuse.skip_completed",
                "跳过已完成归档",
                "不可编辑：旧缓存缺少输出及策略完成证据，核心会忽略此设置并重新处理归档。",
                Kind::ReadOnly
            ),
        ],
    },
    Section {
        label: "资源预算",
        fields: &[
            field!(
                "limits.max_files",
                "最大输出文件数",
                "累计输出条目上限，0 表示不限。",
                Kind::Number
            ),
            field!(
                "limits.max_output_bytes",
                "最大输出字节数",
                "累计输出字节上限，0 表示不限。",
                Kind::Number
            ),
            field!(
                "limits.min_free_bytes",
                "最小剩余空间",
                "可用磁盘字节数下限，0 表示关闭容量检查。",
                Kind::Number
            ),
            field!(
                "limits.max_nested_candidates",
                "最大嵌套候选数",
                "限制嵌套归档扫描产生的候选数量。",
                Kind::Number
            ),
        ],
    },
    Section {
        label: "后端与日志",
        fields: &[
            field!(
                "backends.auto_discover",
                "自动发现后端",
                "允许自动寻找可用后端；后端设置保存后重新构建执行环境。",
                Kind::Bool
            ),
            field!(
                "backends.installations",
                "后端安装列表",
                "TOML 内联表数组；每项包含 id、family（seven-zip-cli/unrar-cli）、executable，可选 declared_version、enabled、priority。",
                Kind::Toml
            ),
            field!(
                "interaction.mode",
                "交互策略",
                "自动、始终允许交互或禁止交互。",
                Kind::Choice(&[("auto", "自动"), ("always", "始终"), ("never", "禁止")])
            ),
            field!(
                "logging.level",
                "日志级别",
                "控制 CLI 终端日志详细程度；GUI 任务进度与事件展示不受此项过滤。",
                Kind::Choice(&[
                    ("off", "关闭"),
                    ("error", "错误"),
                    ("warn", "警告"),
                    ("info", "信息"),
                    ("debug", "调试")
                ])
            ),
            field!(
                "logging.file",
                "写入日志文件",
                "不可编辑：文件日志尚未实现，核心拒绝 logging.file=true。",
                Kind::ReadOnly
            ),
        ],
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use smartzip_config::{value_at, ResolvedConfig};
    use std::collections::HashSet;

    #[test]
    fn schema_covers_all_user_configurable_leaves() {
        let resolved = ResolvedConfig::load(None).unwrap();
        let value = toml::Value::try_from(&resolved.values).unwrap();
        let optional = ["state.database", "extraction.output.directory"];
        let mut keys = HashSet::new();
        for field in SECTIONS.iter().flat_map(|section| section.fields) {
            assert!(keys.insert(field.key), "duplicate field {}", field.key);
            assert!(
                value_at(&value, field.key).is_some() || optional.contains(&field.key),
                "missing key {}",
                field.key
            );
        }
        fn check_leaves(value: &toml::Value, prefix: &str, keys: &HashSet<&str>) {
            if let Some(table) = value.as_table() {
                for (key, value) in table {
                    let key = if prefix.is_empty() {
                        key.clone()
                    } else {
                        format!("{prefix}.{key}")
                    };
                    check_leaves(value, &key, keys);
                }
            } else if !["schema_version", "defaults_version"].contains(&prefix) {
                assert!(
                    keys.contains(prefix),
                    "config leaf missing from GUI: {prefix}"
                );
            }
        }
        check_leaves(&value, "", &keys);
    }

    #[test]
    fn every_choice_is_accepted_by_shared_config() {
        for field in SECTIONS.iter().flat_map(|section| section.fields) {
            let Kind::Choice(options) = field.kind else {
                continue;
            };
            for (option, _) in options {
                let mut resolved = ResolvedConfig::load(None).unwrap();
                let mut patch = vec![(field.key.into(), toml::Value::String((*option).into()))];
                if field.key == "extraction.output.destination" && *option == "directory" {
                    patch.push((
                        "extraction.output.directory".into(),
                        toml::Value::String("output".into()),
                    ));
                }
                resolved
                    .apply(&patch)
                    .unwrap_or_else(|error| panic!("{}={option}: {error}", field.key));
            }
        }
    }
}
