//! Canonical group-free flat Aura Plan v2 (`AUF2`).
//!
//! Registry 1 is the frozen fixed/absolute-varint contract, registry 2 adds
//! exact-byte variable dictionaries, and registry 3 additively authorizes
//! per-chunk temporal lanes only for the schema-stamped primary timestamp.

use sha2::{Digest, Sha256};

use crate::v3_codecs::{integer_varint_codec, PlanV2PhysicalCodec};
use crate::{canonical_v3_schema_fingerprint, AuraError, Result, SchemaDescriptor};

pub const FLAT_PLAN_V2_MAGIC: &[u8; 4] = b"AUF2";
pub const FLAT_PLAN_V2_VERSION: u16 = 2;
pub const FLAT_PLAN_V2_REGISTRY_VERSION: u16 = 1;
pub const FLAT_PLAN_V2_DICTIONARY_REGISTRY_VERSION: u16 = 2;
pub const FLAT_PLAN_V2_TEMPORAL_REGISTRY_VERSION: u16 = 3;
pub const MAX_FLAT_PLAN_V2_BYTES: usize = 16 * 1024 * 1024;
const HEADER_BYTES: usize = 52;
const HASH_BYTES: usize = 32;
const REGISTRY1_HASH_DOMAIN: &[u8] = b"aura-flat-plan-v2-registry-v1\0";
const REGISTRY2_HASH_DOMAIN: &[u8] = b"aura-flat-plan-v2-registry-v2\0";
const REGISTRY3_HASH_DOMAIN: &[u8] = b"aura-flat-plan-v2-registry-v3\0";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlatAuraPlanV2 {
    pub plan_version: u16,
    pub registry_version: u16,
    pub schema_id: u32,
    pub schema_fingerprint: [u8; 32],
    pub codecs: Vec<PlanV2PhysicalCodec>,
}

impl FlatAuraPlanV2 {
    pub fn all_fixed(schema: &SchemaDescriptor) -> Result<Self> {
        crate::v3_container::validate_flat_schema(schema)?;
        let plan = Self {
            plan_version: FLAT_PLAN_V2_VERSION,
            registry_version: FLAT_PLAN_V2_REGISTRY_VERSION,
            schema_id: schema.schema_id,
            schema_fingerprint: canonical_v3_schema_fingerprint(schema)?,
            codecs: vec![PlanV2PhysicalCodec::FixedWidth; schema.fields.len()],
        };
        plan.validate(schema)?;
        Ok(plan)
    }

    pub fn validate(&self, schema: &SchemaDescriptor) -> Result<()> {
        crate::v3_container::validate_flat_schema(schema)?;
        if self.plan_version != FLAT_PLAN_V2_VERSION
            || self.schema_id != schema.schema_id
            || self.schema_fingerprint != canonical_v3_schema_fingerprint(schema)?
            || self.codecs.len() != schema.fields.len()
        {
            return Err(AuraError::InvalidValue("flat plan v2 contract"));
        }
        match self.registry_version {
            FLAT_PLAN_V2_REGISTRY_VERSION => {
                for (field, codec) in schema.fields.iter().zip(&self.codecs) {
                    if *codec != PlanV2PhysicalCodec::FixedWidth
                        && integer_varint_codec(field.field_type) != Some(*codec)
                    {
                        return Err(AuraError::InvalidValue("flat plan v2 codec type"));
                    }
                }
            }
            FLAT_PLAN_V2_DICTIONARY_REGISTRY_VERSION => {
                let mut has_dictionary = false;
                for (field, codec) in schema.fields.iter().zip(&self.codecs) {
                    let valid = match codec {
                        PlanV2PhysicalCodec::FixedWidth => true,
                        PlanV2PhysicalCodec::UnsignedUleb128
                        | PlanV2PhysicalCodec::SignedZigZagUleb128 => {
                            integer_varint_codec(field.field_type) == Some(*codec)
                        }
                        PlanV2PhysicalCodec::VariableByteDictionaryBitpacked => {
                            has_dictionary = true;
                            matches!(
                                field.field_type,
                                crate::FieldType::Utf8 | crate::FieldType::DecimalText
                            )
                        }
                        PlanV2PhysicalCodec::TimestampPreviousDeltaZigZagUleb128
                        | PlanV2PhysicalCodec::TimestampDeltaOfDeltaZigZagUleb128 => false,
                    };
                    if !valid {
                        return Err(AuraError::InvalidValue("flat plan v2 codec type"));
                    }
                }
                if !has_dictionary {
                    return Err(AuraError::InvalidValue("flat plan v2 dictionary registry"));
                }
            }
            FLAT_PLAN_V2_TEMPORAL_REGISTRY_VERSION => {
                let mut has_temporal = false;
                for (field, codec) in schema.fields.iter().zip(&self.codecs) {
                    let valid = match codec {
                        PlanV2PhysicalCodec::FixedWidth => true,
                        PlanV2PhysicalCodec::UnsignedUleb128
                        | PlanV2PhysicalCodec::SignedZigZagUleb128 => {
                            integer_varint_codec(field.field_type) == Some(*codec)
                        }
                        PlanV2PhysicalCodec::VariableByteDictionaryBitpacked => matches!(
                            field.field_type,
                            crate::FieldType::Utf8 | crate::FieldType::DecimalText
                        ),
                        PlanV2PhysicalCodec::TimestampPreviousDeltaZigZagUleb128 => {
                            has_temporal = true;
                            temporal_field_authorized(
                                schema,
                                field,
                                crate::FieldTransform::DeltaPrevious,
                            )
                        }
                        PlanV2PhysicalCodec::TimestampDeltaOfDeltaZigZagUleb128 => {
                            has_temporal = true;
                            temporal_field_authorized(schema, field, crate::FieldTransform::Delta2)
                        }
                    };
                    if !valid {
                        return Err(AuraError::InvalidValue("flat plan v2 codec type"));
                    }
                }
                if !has_temporal {
                    return Err(AuraError::InvalidValue("flat plan v2 temporal registry"));
                }
            }
            _ => return Err(AuraError::InvalidValue("flat plan v2 registry")),
        }
        Ok(())
    }

    pub fn encode(&self, schema: &SchemaDescriptor) -> Result<Vec<u8>> {
        self.validate(schema)?;
        let length = HEADER_BYTES
            .checked_add(self.codecs.len())
            .and_then(|value| value.checked_add(HASH_BYTES))
            .filter(|value| *value <= MAX_FLAT_PLAN_V2_BYTES)
            .ok_or(AuraError::InvalidValue("flat plan v2 length"))?;
        let mut out = Vec::new();
        out.try_reserve_exact(length)
            .map_err(|_| AuraError::InvalidValue("flat plan v2 allocation"))?;
        out.extend_from_slice(FLAT_PLAN_V2_MAGIC);
        put_u16(&mut out, self.plan_version);
        put_u16(&mut out, self.registry_version);
        put_u32(&mut out, length as u32);
        put_u32(&mut out, self.schema_id);
        out.extend_from_slice(&self.schema_fingerprint);
        put_u16(&mut out, self.codecs.len() as u16);
        put_u16(&mut out, 0);
        debug_assert_eq!(out.len(), HEADER_BYTES);
        out.extend(self.codecs.iter().map(|codec| *codec as u8));
        let hash = hash_bytes(self.registry_version, &out)?;
        out.extend_from_slice(&hash);
        Ok(out)
    }

    pub fn decode(schema: &SchemaDescriptor, bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_FLAT_PLAN_V2_BYTES || bytes.len() < HEADER_BYTES + HASH_BYTES {
            return Err(AuraError::InvalidValue("flat plan v2 length"));
        }
        if &bytes[..4] != FLAT_PLAN_V2_MAGIC {
            return Err(AuraError::InvalidMagic { expected: "AUF2" });
        }
        let plan_version = u16::from_le_bytes(bytes[4..6].try_into().unwrap());
        let registry_version = u16::from_le_bytes(bytes[6..8].try_into().unwrap());
        let hash_start = bytes.len() - HASH_BYTES;
        if hash_bytes(registry_version, &bytes[..hash_start])? != bytes[hash_start..] {
            return Err(AuraError::InvalidValue("flat plan v2 hash"));
        }
        let length = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
        let schema_id = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
        let schema_fingerprint = bytes[16..48].try_into().unwrap();
        let count = u16::from_le_bytes(bytes[48..50].try_into().unwrap()) as usize;
        if length != bytes.len()
            || bytes[50..52] != [0, 0]
            || count > hash_start.saturating_sub(HEADER_BYTES)
            || HEADER_BYTES + count != hash_start
        {
            return Err(AuraError::InvalidValue("flat plan v2 layout"));
        }
        let mut codecs = Vec::new();
        codecs
            .try_reserve_exact(count)
            .map_err(|_| AuraError::InvalidValue("flat plan v2 allocation"))?;
        for code in &bytes[HEADER_BYTES..hash_start] {
            codecs.push(PlanV2PhysicalCodec::from_code(*code)?);
        }
        let plan = Self {
            plan_version,
            registry_version,
            schema_id,
            schema_fingerprint,
            codecs,
        };
        plan.validate(schema)?;
        if plan.encode(schema)? != bytes {
            return Err(AuraError::InvalidValue("flat plan v2 noncanonical"));
        }
        Ok(plan)
    }

    pub fn hash(&self, schema: &SchemaDescriptor) -> Result<[u8; 32]> {
        let bytes = self.encode(schema)?;
        Ok(bytes[bytes.len() - HASH_BYTES..].try_into().unwrap())
    }
}

fn hash_bytes(registry_version: u16, bytes: &[u8]) -> Result<[u8; 32]> {
    let domain = match registry_version {
        FLAT_PLAN_V2_REGISTRY_VERSION => REGISTRY1_HASH_DOMAIN,
        FLAT_PLAN_V2_DICTIONARY_REGISTRY_VERSION => REGISTRY2_HASH_DOMAIN,
        FLAT_PLAN_V2_TEMPORAL_REGISTRY_VERSION => REGISTRY3_HASH_DOMAIN,
        _ => return Err(AuraError::InvalidValue("flat plan v2 registry")),
    };
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(
        u64::try_from(bytes.len())
            .map_err(|_| AuraError::InvalidValue("flat plan v2 length"))?
            .to_le_bytes(),
    );
    hasher.update(bytes);
    Ok(hasher.finalize().into())
}

pub(crate) fn temporal_field_authorized(
    schema: &SchemaDescriptor,
    field: &crate::FieldDescriptor,
    transform: crate::FieldTransform,
) -> bool {
    field.index == 0
        && field.role == crate::FieldRole::Timestamp
        && field.scope == crate::FieldScope::Event
        && !field.nullable
        && field.scale == 0
        && field.relation == crate::FieldRelation::None
        && matches!(
            field.field_type,
            crate::FieldType::TimestampNs | crate::FieldType::TimestampMs
        )
        && field.candidates.contains(crate::FieldTransform::Absolute)
        && field.candidates.contains(transform)
        && schema
            .compact_schema_map
            .as_deref()
            .and_then(|mapping| mapping.get(usize::from(field.index)))
            == Some(&100)
}

fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}
fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod temporal_authorization_tests {
    use super::*;
    use crate::{FieldRelation, FieldRole, FieldScope, FieldTransform, FieldType, SchemaBuilder};

    #[test]
    fn temporal_authorization_rejects_scale_relation_and_scope_gates() {
        let schema = SchemaBuilder::new("anonymous_temporal_gates")
            .v3()
            .field("clock", FieldType::TimestampNs, FieldRole::Timestamp)
            .finish()
            .unwrap();
        assert!(temporal_field_authorized(
            &schema,
            &schema.fields[0],
            FieldTransform::DeltaPrevious
        ));

        let mutations: [fn(&mut crate::FieldDescriptor); 3] = [
            |field: &mut crate::FieldDescriptor| field.scale = 1,
            |field: &mut crate::FieldDescriptor| field.relation = FieldRelation::DeltaFromField(0),
            |field: &mut crate::FieldDescriptor| field.scope = FieldScope::Repeated,
        ];
        for mutate in mutations {
            let mut field = schema.fields[0].clone();
            mutate(&mut field);
            assert!(!temporal_field_authorized(
                &schema,
                &field,
                FieldTransform::DeltaPrevious
            ));
        }
    }
}
