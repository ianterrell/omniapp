use serde::Serialize;

use crate::Record;

#[derive(Debug, Clone, Serialize)]
pub struct RelationshipLink {
    /// The reference field on the record that owns the relationship.
    pub field: String,
    pub target_field: String,
    /// The record on the other side of the relationship.
    pub record: Record,
}

#[derive(Debug, Clone, Serialize)]
pub struct RelationshipSet {
    pub record: Record,
    pub outbound: Vec<RelationshipLink>,
    pub inbound: Vec<RelationshipLink>,
}
