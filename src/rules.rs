//! 校验管理员配置，按不区分大小写的头名替换已有字段

use std::collections::HashSet;

use gateway_plugin_sdk::call::middleware::MiddlewareHeader;
use http::{HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};

/// 单个请求头覆写项，值可能包含敏感信息
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeaderOverride {
    pub name: String,
    pub value: String,
}

/// 一个账号的完整规则，停用时保留编辑内容
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub enabled: bool,
    pub headers: Vec<HeaderOverride>,
}

impl Rule {
    /// 校验头名、头值与存储配额，不将敏感值写入错误
    /// @return 规范化后的头名，或可展示给管理员的中文错误
    pub fn normalized(mut self) -> Result<Self, &'static str> {
        if self.headers.len() > 64 {
            return Err("每个账号最多配置 64 个请求头");
        }
        let mut names = HashSet::new();
        for header in &mut self.headers {
            let name = HeaderName::from_bytes(header.name.trim().as_bytes())
                .map_err(|_| "请求头名称无效")?;
            HeaderValue::from_bytes(header.value.as_bytes()).map_err(|_| "请求头值包含非法字符")?;
            header.name = name.as_str().to_owned();
            if !names.insert(header.name.clone()) {
                return Err("请求头名称重复，名称不区分大小写");
            }
        }
        if serde_json::to_vec(&self).map_err(|_| "规则编码失败")?.len() > 65_536 {
            return Err("单个账号的请求头配置超过 64 KiB");
        }
        Ok(self)
    }

    /// 覆盖全部同名字段并追加缺失字段，保留其他字段的字节与顺序
    /// @param headers Provider 和下层中间件生成的请求头
    /// @return 无，直接修改请求头集合
    pub fn apply(&self, headers: &mut Vec<MiddlewareHeader>) {
        if !self.enabled || self.headers.is_empty() {
            return;
        }
        // 原请求可能有多项同名字段，必须全部移除才能实现明确的覆写语义
        headers.retain(|existing| {
            !self
                .headers
                .iter()
                .any(|header| header.name.eq_ignore_ascii_case(&existing.name))
        });
        headers.extend(self.headers.iter().map(|header| MiddlewareHeader {
            name: header.name.clone(),
            value: header.value.as_bytes().to_vec(),
        }));
    }
}
