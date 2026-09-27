use serde::de::{self, DeserializeSeed, IntoDeserializer, MapAccess, SeqAccess, Visitor};
use serde::Deserialize;
use std::cell::RefCell;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leaf {
    Bool,
    Integer,
    Float,
    String,
    Array,
    Any,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Table(BTreeMap<String, Node>),
    Leaf(Leaf),
}

impl Node {
    pub fn children(&self) -> Option<&BTreeMap<String, Node>> {
        match self {
            Node::Table(children) => Some(children),
            Node::Leaf(_) => None,
        }
    }

    pub fn at(&self, path: &[&str]) -> Option<&Node> {
        path.iter()
            .try_fold(self, |node, key| node.children()?.get(*key))
    }

    pub fn leaf_paths(&self) -> Vec<String> {
        let mut out = Vec::new();
        collect_leaf_paths(self, "", &mut out);
        out
    }
}

fn collect_leaf_paths(node: &Node, prefix: &str, out: &mut Vec<String>) {
    match node {
        Node::Leaf(_) => out.push(prefix.to_string()),
        Node::Table(children) => {
            for (key, child) in children {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                collect_leaf_paths(child, &path, out);
            }
        }
    }
}

pub fn config_schema() -> &'static Node {
    static SCHEMA: once_cell::sync::Lazy<Node> =
        once_cell::sync::Lazy::new(schema_of::<super::Config>);
    &SCHEMA
}

pub fn schema_of<'de, T: Deserialize<'de>>() -> Node {
    let slot = RefCell::new(None);
    T::deserialize(Probe { slot: &slot }).expect("probing a config struct cannot fail");
    slot.into_inner().unwrap_or(Node::Leaf(Leaf::Any))
}

struct Probe<'a> {
    slot: &'a RefCell<Option<Node>>,
}

impl Probe<'_> {
    fn record(&self, leaf: Leaf) {
        *self.slot.borrow_mut() = Some(Node::Leaf(leaf));
    }
}

type ProbeError = de::value::Error;

macro_rules! probe_leaf {
    ($method:ident, $visit:ident, $value:expr, $leaf:expr) => {
        fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ProbeError> {
            self.record($leaf);
            visitor.$visit($value)
        }
    };
}

impl<'de> de::Deserializer<'de> for Probe<'_> {
    type Error = ProbeError;

    probe_leaf!(deserialize_any, visit_i64, 0, Leaf::Any);
    probe_leaf!(deserialize_bool, visit_bool, false, Leaf::Bool);
    probe_leaf!(deserialize_i8, visit_i8, 0, Leaf::Integer);
    probe_leaf!(deserialize_i16, visit_i16, 0, Leaf::Integer);
    probe_leaf!(deserialize_i32, visit_i32, 0, Leaf::Integer);
    probe_leaf!(deserialize_i64, visit_i64, 0, Leaf::Integer);
    probe_leaf!(deserialize_u8, visit_u8, 0, Leaf::Integer);
    probe_leaf!(deserialize_u16, visit_u16, 0, Leaf::Integer);
    probe_leaf!(deserialize_u32, visit_u32, 0, Leaf::Integer);
    probe_leaf!(deserialize_u64, visit_u64, 0, Leaf::Integer);
    probe_leaf!(deserialize_f32, visit_f32, 0.0, Leaf::Float);
    probe_leaf!(deserialize_f64, visit_f64, 0.0, Leaf::Float);
    probe_leaf!(deserialize_char, visit_char, 'a', Leaf::String);
    probe_leaf!(deserialize_str, visit_str, "", Leaf::String);
    probe_leaf!(deserialize_string, visit_str, "", Leaf::String);
    probe_leaf!(deserialize_bytes, visit_bytes, &[], Leaf::Any);
    probe_leaf!(deserialize_byte_buf, visit_bytes, &[], Leaf::Any);
    probe_leaf!(deserialize_identifier, visit_str, "", Leaf::String);

    fn deserialize_unit<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ProbeError> {
        self.record(Leaf::Any);
        visitor.visit_unit()
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ProbeError> {
        self.record(Leaf::Any);
        visitor.visit_unit()
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, ProbeError> {
        self.deserialize_unit(visitor)
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ProbeError> {
        visitor.visit_some(self)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, ProbeError> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ProbeError> {
        self.record(Leaf::Array);
        visitor.visit_seq(EmptySeq)
    }

    fn deserialize_tuple<V: Visitor<'de>>(
        self,
        _len: usize,
        visitor: V,
    ) -> Result<V::Value, ProbeError> {
        self.deserialize_seq(visitor)
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _len: usize,
        visitor: V,
    ) -> Result<V::Value, ProbeError> {
        self.deserialize_seq(visitor)
    }

    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ProbeError> {
        self.record(Leaf::Any);
        visitor.visit_map(StructFields {
            fields: &[],
            next: 0,
            table: &RefCell::new(BTreeMap::new()),
        })
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, ProbeError> {
        let table = RefCell::new(BTreeMap::new());
        let value = visitor.visit_map(StructFields {
            fields,
            next: 0,
            table: &table,
        })?;
        *self.slot.borrow_mut() = Some(Node::Table(table.into_inner()));
        Ok(value)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, ProbeError> {
        self.record(Leaf::String);
        let first: de::value::StrDeserializer<'_, ProbeError> =
            variants.first().copied().unwrap_or("").into_deserializer();
        visitor.visit_enum(first)
    }
}

struct EmptySeq;

impl<'de> SeqAccess<'de> for EmptySeq {
    type Error = ProbeError;

    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        _seed: T,
    ) -> Result<Option<T::Value>, ProbeError> {
        Ok(None)
    }
}

struct StructFields<'a> {
    fields: &'static [&'static str],
    next: usize,
    table: &'a RefCell<BTreeMap<String, Node>>,
}

impl<'de> MapAccess<'de> for StructFields<'_> {
    type Error = ProbeError;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, ProbeError> {
        let Some(field) = self.fields.get(self.next) else {
            return Ok(None);
        };
        let key: de::value::StrDeserializer<'_, ProbeError> = field.into_deserializer();
        seed.deserialize(key).map(Some)
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(
        &mut self,
        seed: V,
    ) -> Result<V::Value, ProbeError> {
        let field = self.fields[self.next];
        self.next += 1;
        let slot = RefCell::new(None);
        let value = seed.deserialize(Probe { slot: &slot })?;
        self.table.borrow_mut().insert(
            field.to_string(),
            slot.into_inner().unwrap_or(Node::Leaf(Leaf::Any)),
        );
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::{config_schema, Leaf, Node};

    #[test]
    fn the_schema_names_optional_and_nested_fields_and_skips_runtime_ones() {
        let schema = config_schema();
        let leaf = |path: &str| schema.at(&path.split('.').collect::<Vec<_>>()).cloned();
        assert_eq!(leaf("hook.quality"), Some(Node::Leaf(Leaf::Integer)));
        assert_eq!(leaf("hook.threshold"), Some(Node::Leaf(Leaf::Float)));
        assert_eq!(leaf("embed.prefix_scheme"), Some(Node::Leaf(Leaf::String)));
        assert_eq!(leaf("backup.s3.bucket"), Some(Node::Leaf(Leaf::String)));
        assert_eq!(
            leaf("mcp.weights.transcript"),
            Some(Node::Leaf(Leaf::Float))
        );
        assert_eq!(leaf("weights.decay.floor"), Some(Node::Leaf(Leaf::Float)));
        assert_eq!(leaf("sources"), Some(Node::Leaf(Leaf::Array)));
        assert_eq!(leaf("pdf.ocr"), Some(Node::Leaf(Leaf::String)));
        assert_eq!(
            leaf("index_transcripts_max_age_days"),
            Some(Node::Leaf(Leaf::Integer))
        );
        assert_eq!(leaf("embed.remote"), None);
        assert_eq!(leaf("embed.remote_error"), None);
    }
}
