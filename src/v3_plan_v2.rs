//! Canonical Aura Plan v2 registry for planned grouped Aura0 V3 files.
//!
//! Registry version 1 stamps attempt-1 exact direct streams. Registry version 2
//! adds plan-bound compact physical stream identities plus the
//! schema-authorized split-domain direct candidate. Complete-cost selection
//! retains registry-1 and compact registry-2 direct fallbacks. This registry
//! specifies no compression and carries no Parquet, holdout, or production
//! claim.

use sha2::{Digest, Sha256};

use crate::schema::{FieldScope, FieldType, SchemaDescriptor};
use crate::v3_values::canonical_v3_schema_fingerprint;
use crate::{AuraError, Result};

pub const AURA_PLAN_V2_MAGIC: &[u8; 4] = b"AUP2";
pub const AURA_PLAN_V2_VERSION: u16 = 2;
pub const AURA_PLAN_V2_REGISTRY_VERSION: u16 = 1;
pub const AURA_PLAN_V2_SPLIT_REGISTRY_VERSION: u16 = 2;
pub const AURA_PLAN_V2_DIRECT_OP: u8 = 0;
pub const AURA_PLAN_V2_SPLIT_DOMAIN_DIRECT_OP: u8 = 1;
pub const AURA_PLAN_V2_AUTHORITATIVE_SOURCE_ORDER: u8 = 1;
pub const MAX_AURA_PLAN_V2_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_AURA_PLAN_V2_STREAMS: usize = 4_096;
pub const MAX_AURA_PLAN_V2_DEPENDENCIES: usize = 4_096;

const HEADER_BYTES: usize = 64;
const STREAM_PREFIX_BYTES: usize = 12;
const SPLIT_STREAM_PREFIX_BYTES: usize = 14;
const HASH_BYTES: usize = 32;
const PLAN_HASH_DOMAIN: &[u8] = b"aura-plan-v2-registry-v1\0";
const SPLIT_PLAN_HASH_DOMAIN: &[u8] = b"aura-plan-v2-registry-v2\0";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PlanV2Selection {
    Direct = 0,
    SplitDomainDirect = 1,
}

impl PlanV2Selection {
    fn from_code(code: u8) -> Result<Self> {
        match code {
            0 => Ok(Self::Direct),
            1 => Ok(Self::SplitDomainDirect),
            _ => Err(AuraError::InvalidValue("aura plan v2 selection")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanV2StreamDescriptor {
    pub slot: u16,
    pub scope: FieldScope,
    pub field_type: FieldType,
    pub nullable: bool,
    pub op: u8,
    pub dependencies: Vec<u16>,
    pub physical_stream_ids: Vec<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuraPlanV2 {
    pub plan_version: u16,
    pub registry_version: u16,
    pub schema_id: u32,
    pub schema_fingerprint: [u8; 32],
    pub group_id: u16,
    pub discriminator_slot: u16,
    pub source_order: u8,
    pub selection: PlanV2Selection,
    pub authorized_relationship_bits: u8,
    pub event_child_offsets_stream_id: Option<u16>,
    pub source_order_selector_stream_id: Option<u16>,
    pub streams: Vec<PlanV2StreamDescriptor>,
    pub decode_order: Vec<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanV2Inspection {
    pub plan_version: u16,
    pub registry_version: u16,
    pub schema_id: u32,
    pub selected: PlanV2Selection,
    pub stream_count: u16,
    pub discriminator_slot: u16,
    pub authoritative_source_order: bool,
    pub authorized_relationship_bits: u8,
    pub relationships_attempted: bool,
}

impl AuraPlanV2 {
    pub fn direct_for_schema(schema: &SchemaDescriptor) -> Result<Self> {
        crate::v3_events::validate_v3_grouped_exact_subset(schema)?;
        let group = schema
            .groups
            .first()
            .ok_or(AuraError::InvalidValue("aura plan v2 group"))?;
        let discriminator_slot = group
            .dual_domain
            .ok_or(AuraError::InvalidValue("aura plan v2 discriminator"))?
            .discriminator_slot;
        let mut streams = Vec::new();
        streams
            .try_reserve_exact(schema.fields.len())
            .map_err(|_| AuraError::InvalidValue("aura plan v2 allocation"))?;
        let mut decode_order = Vec::new();
        decode_order
            .try_reserve_exact(schema.fields.len())
            .map_err(|_| AuraError::InvalidValue("aura plan v2 allocation"))?;
        for field in &schema.fields {
            streams.push(PlanV2StreamDescriptor {
                slot: field.index,
                scope: field.scope,
                field_type: field.field_type,
                nullable: field.nullable,
                op: AURA_PLAN_V2_DIRECT_OP,
                dependencies: Vec::new(),
                physical_stream_ids: Vec::new(),
            });
            decode_order.push(field.index);
        }
        let plan = Self {
            plan_version: AURA_PLAN_V2_VERSION,
            registry_version: AURA_PLAN_V2_REGISTRY_VERSION,
            schema_id: schema.schema_id,
            schema_fingerprint: canonical_v3_schema_fingerprint(schema)?,
            group_id: group.group_id,
            discriminator_slot,
            source_order: AURA_PLAN_V2_AUTHORITATIVE_SOURCE_ORDER,
            selection: PlanV2Selection::Direct,
            authorized_relationship_bits: group.relationships.bits(),
            event_child_offsets_stream_id: None,
            source_order_selector_stream_id: None,
            streams,
            decode_order,
        };
        plan.validate(schema)?;
        Ok(plan)
    }

    pub fn candidate_for_schema(
        schema: &SchemaDescriptor,
        selection: PlanV2Selection,
    ) -> Result<Self> {
        let mut plan = Self::direct_for_schema(schema)?;
        plan.registry_version = AURA_PLAN_V2_SPLIT_REGISTRY_VERSION;
        plan.selection = selection;
        plan.event_child_offsets_stream_id = Some(0);
        let mut physical_id = 1u16;
        let physical_order = physical_stream_order(&plan);
        for index in physical_order {
            let stream = &mut plan.streams[index];
            stream.physical_stream_ids = vec![physical_id];
            if stream.slot == plan.discriminator_slot {
                plan.source_order_selector_stream_id = Some(physical_id);
            }
            physical_id = physical_id
                .checked_add(1)
                .ok_or(AuraError::InvalidValue("aura plan v2 physical stream id"))?;
            if selection == PlanV2Selection::SplitDomainDirect
                && stream.scope == FieldScope::Repeated
                && stream.slot != plan.discriminator_slot
            {
                stream.op = AURA_PLAN_V2_SPLIT_DOMAIN_DIRECT_OP;
                stream.dependencies = vec![plan.discriminator_slot];
                stream.physical_stream_ids.push(physical_id);
                physical_id = physical_id
                    .checked_add(1)
                    .ok_or(AuraError::InvalidValue("aura plan v2 physical stream id"))?;
            }
        }
        plan.validate(schema)?;
        Ok(plan)
    }

    pub fn validate(&self, schema: &SchemaDescriptor) -> Result<()> {
        crate::v3_events::validate_v3_grouped_exact_subset(schema)?;
        if self.plan_version != AURA_PLAN_V2_VERSION
            || !matches!(
                self.registry_version,
                AURA_PLAN_V2_REGISTRY_VERSION | AURA_PLAN_V2_SPLIT_REGISTRY_VERSION
            )
            || self.schema_id != schema.schema_id
            || self.schema_fingerprint != canonical_v3_schema_fingerprint(schema)?
            || self.source_order != AURA_PLAN_V2_AUTHORITATIVE_SOURCE_ORDER
            || self.streams.len() != schema.fields.len()
            || self.decode_order.len() != schema.fields.len()
            || self.streams.len() > MAX_AURA_PLAN_V2_STREAMS
        {
            return Err(AuraError::InvalidValue("aura plan v2 contract"));
        }
        let group = schema
            .groups
            .first()
            .ok_or(AuraError::InvalidValue("aura plan v2 group"))?;
        let discriminator_slot = group
            .dual_domain
            .ok_or(AuraError::InvalidValue("aura plan v2 discriminator"))?
            .discriminator_slot;
        if self.group_id != group.group_id
            || self.discriminator_slot != discriminator_slot
            || self.authorized_relationship_bits != group.relationships.bits()
        {
            return Err(AuraError::InvalidValue("aura plan v2 authorization"));
        }
        if self.selection == PlanV2Selection::SplitDomainDirect
            && !group.relationships.allows_split()
        {
            return Err(AuraError::InvalidValue("aura plan v2 split authorization"));
        }
        if self.registry_version == AURA_PLAN_V2_SPLIT_REGISTRY_VERSION
            && (self.event_child_offsets_stream_id != Some(0)
                || self.source_order_selector_stream_id.is_none())
        {
            return Err(AuraError::InvalidValue("aura plan v2 structural streams"));
        }
        if self.registry_version == AURA_PLAN_V2_REGISTRY_VERSION
            && (self.event_child_offsets_stream_id.is_some()
                || self.source_order_selector_stream_id.is_some())
        {
            return Err(AuraError::InvalidValue("aura plan v2 direct registry"));
        }

        let mut seen = vec![false; schema.fields.len()];
        for (position, descriptor) in self.streams.iter().enumerate() {
            let index = usize::from(descriptor.slot);
            let field = schema
                .fields
                .get(index)
                .filter(|field| field.index == descriptor.slot)
                .ok_or(AuraError::InvalidValue("aura plan v2 stream slot"))?;
            if seen[index] || index != position {
                return Err(AuraError::InvalidValue("aura plan v2 duplicate stream"));
            }
            seen[index] = true;
            if descriptor.scope != field.scope
                || descriptor.field_type != field.field_type
                || descriptor.nullable != field.nullable
                || descriptor.dependencies.len() > MAX_AURA_PLAN_V2_DEPENDENCIES
            {
                return Err(AuraError::InvalidValue("aura plan v2 stream schema"));
            }
        }
        validate_acyclic(&self.streams, schema.fields.len())?;
        if self.registry_version == AURA_PLAN_V2_REGISTRY_VERSION {
            if self.selection != PlanV2Selection::Direct {
                return Err(AuraError::InvalidValue("aura plan v2 direct selection"));
            }
            if self.streams.iter().any(|descriptor| {
                descriptor.op != AURA_PLAN_V2_DIRECT_OP
                    || !descriptor.dependencies.is_empty()
                    || !descriptor.physical_stream_ids.is_empty()
            }) {
                return Err(AuraError::InvalidValue("aura plan v2 direct registry"));
            }
        } else {
            let mut next_physical = 1u16;
            for index in physical_stream_order(self) {
                let descriptor = &self.streams[index];
                let split = self.selection == PlanV2Selection::SplitDomainDirect
                    && descriptor.scope == FieldScope::Repeated
                    && descriptor.slot != self.discriminator_slot;
                let expected_op = if split {
                    AURA_PLAN_V2_SPLIT_DOMAIN_DIRECT_OP
                } else {
                    AURA_PLAN_V2_DIRECT_OP
                };
                let expected_dependencies = if split {
                    &[self.discriminator_slot][..]
                } else {
                    &[][..]
                };
                let expected_streams = if split { 2 } else { 1 };
                if descriptor.op != expected_op
                    || descriptor.dependencies != expected_dependencies
                    || descriptor.physical_stream_ids.len() != expected_streams
                {
                    return Err(AuraError::InvalidValue("aura plan v2 split registry"));
                }
                if descriptor.slot == self.discriminator_slot
                    && descriptor.physical_stream_ids.first().copied()
                        != self.source_order_selector_stream_id
                {
                    return Err(AuraError::InvalidValue("aura plan v2 selector stream id"));
                }
                for physical in &descriptor.physical_stream_ids {
                    if *physical != next_physical {
                        return Err(AuraError::InvalidValue("aura plan v2 physical stream id"));
                    }
                    next_physical = next_physical
                        .checked_add(1)
                        .ok_or(AuraError::InvalidValue("aura plan v2 physical stream id"))?;
                }
            }
        }
        for (index, slot) in self.decode_order.iter().copied().enumerate() {
            if usize::from(slot) != index {
                return Err(AuraError::InvalidValue("aura plan v2 decode order"));
            }
        }
        Ok(())
    }

    pub fn inspection(&self) -> PlanV2Inspection {
        PlanV2Inspection {
            plan_version: self.plan_version,
            registry_version: self.registry_version,
            schema_id: self.schema_id,
            selected: self.selection,
            stream_count: u16::try_from(self.streams.len()).unwrap_or(u16::MAX),
            discriminator_slot: self.discriminator_slot,
            authoritative_source_order: self.source_order
                == AURA_PLAN_V2_AUTHORITATIVE_SOURCE_ORDER,
            authorized_relationship_bits: self.authorized_relationship_bits,
            relationships_attempted: self.selection == PlanV2Selection::SplitDomainDirect,
        }
    }

    pub fn encode(&self, schema: &SchemaDescriptor) -> Result<Vec<u8>> {
        self.validate(schema)?;
        let dependency_count = self.streams.iter().try_fold(0usize, |total, stream| {
            total
                .checked_add(stream.dependencies.len())
                .ok_or(AuraError::InvalidValue("aura plan v2 length"))
        })?;
        let physical_count = self.streams.iter().try_fold(0usize, |total, stream| {
            total
                .checked_add(stream.physical_stream_ids.len())
                .ok_or(AuraError::InvalidValue("aura plan v2 length"))
        })?;
        let stream_prefix = if self.registry_version == AURA_PLAN_V2_REGISTRY_VERSION {
            STREAM_PREFIX_BYTES
        } else {
            SPLIT_STREAM_PREFIX_BYTES
        };
        let length = HEADER_BYTES
            .checked_add(
                self.streams
                    .len()
                    .checked_mul(stream_prefix)
                    .ok_or(AuraError::InvalidValue("aura plan v2 length"))?,
            )
            .and_then(|value| value.checked_add(dependency_count.checked_mul(2)?))
            .and_then(|value| value.checked_add(physical_count.checked_mul(2)?))
            .and_then(|value| value.checked_add(self.decode_order.len().checked_mul(2)?))
            .and_then(|value| value.checked_add(HASH_BYTES))
            .filter(|value| *value <= MAX_AURA_PLAN_V2_BYTES)
            .ok_or(AuraError::InvalidValue("aura plan v2 length"))?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(length)
            .map_err(|_| AuraError::InvalidValue("aura plan v2 allocation"))?;
        bytes.extend_from_slice(AURA_PLAN_V2_MAGIC);
        put_u16(&mut bytes, self.plan_version);
        put_u16(&mut bytes, self.registry_version);
        put_u16(&mut bytes, self.event_child_offsets_stream_id.unwrap_or(0));
        put_u16(
            &mut bytes,
            self.source_order_selector_stream_id.unwrap_or(0),
        );
        put_u32_len(&mut bytes, length, "aura plan v2 length")?;
        put_u32(&mut bytes, self.schema_id);
        bytes.extend_from_slice(&self.schema_fingerprint);
        put_u16(&mut bytes, self.group_id);
        put_u16(&mut bytes, self.discriminator_slot);
        bytes.push(self.source_order);
        bytes.push(self.selection as u8);
        bytes.push(self.authorized_relationship_bits);
        bytes.push(0);
        put_u16_len(&mut bytes, self.streams.len(), "aura plan v2 stream count")?;
        put_u16_len(
            &mut bytes,
            self.decode_order.len(),
            "aura plan v2 decode count",
        )?;
        debug_assert_eq!(bytes.len(), HEADER_BYTES);
        for stream in &self.streams {
            put_u16(&mut bytes, stream.slot);
            bytes.push(stream.scope as u8);
            bytes.push(stream.field_type as u8);
            bytes.push(u8::from(stream.nullable));
            bytes.push(stream.op);
            put_u16_len(
                &mut bytes,
                stream.dependencies.len(),
                "aura plan v2 dependency count",
            )?;
            put_u32(&mut bytes, 0);
            if self.registry_version == AURA_PLAN_V2_SPLIT_REGISTRY_VERSION {
                put_u16_len(
                    &mut bytes,
                    stream.physical_stream_ids.len(),
                    "aura plan v2 physical stream count",
                )?;
            }
            for dependency in &stream.dependencies {
                put_u16(&mut bytes, *dependency);
            }
            for physical in &stream.physical_stream_ids {
                put_u16(&mut bytes, *physical);
            }
        }
        for slot in &self.decode_order {
            put_u16(&mut bytes, *slot);
        }
        let hash = plan_hash_bytes(&bytes, self.registry_version)?;
        bytes.extend_from_slice(&hash);
        debug_assert_eq!(bytes.len(), length);
        Ok(bytes)
    }

    pub fn decode(schema: &SchemaDescriptor, bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_AURA_PLAN_V2_BYTES {
            return Err(AuraError::InvalidValue("aura plan v2 length"));
        }
        if bytes.len() < HEADER_BYTES + HASH_BYTES {
            return Err(AuraError::UnexpectedEof);
        }
        let hash_start = bytes.len() - HASH_BYTES;
        let wire_registry = u16::from_le_bytes(bytes[6..8].try_into().unwrap());
        if plan_hash_bytes(&bytes[..hash_start], wire_registry)? != bytes[hash_start..] {
            return Err(AuraError::InvalidValue("aura plan v2 hash"));
        }
        let mut reader = Reader::new(&bytes[..hash_start]);
        if reader.take(4)? != AURA_PLAN_V2_MAGIC {
            return Err(AuraError::InvalidMagic { expected: "AUP2" });
        }
        let plan_version = reader.u16()?;
        let registry_version = reader.u16()?;
        if plan_version != AURA_PLAN_V2_VERSION
            || !matches!(
                registry_version,
                AURA_PLAN_V2_REGISTRY_VERSION | AURA_PLAN_V2_SPLIT_REGISTRY_VERSION
            )
        {
            return Err(AuraError::InvalidValue("aura plan v2 registry"));
        }
        let offsets_stream_raw = reader.u16()?;
        let selector_stream_raw = reader.u16()?;
        if reader.u32()? as usize != bytes.len() {
            return Err(AuraError::InvalidValue("aura plan v2 length"));
        }
        let schema_id = reader.u32()?;
        let schema_fingerprint = reader.array32()?;
        let group_id = reader.u16()?;
        let discriminator_slot = reader.u16()?;
        let source_order = reader.u8()?;
        let selection = PlanV2Selection::from_code(reader.u8()?)?;
        let authorized_relationship_bits = reader.u8()?;
        if reader.u8()? != 0 {
            return Err(AuraError::InvalidValue("aura plan v2 reserved"));
        }
        let stream_count = reader.u16()? as usize;
        let decode_count = reader.u16()? as usize;
        let stream_prefix = if registry_version == AURA_PLAN_V2_REGISTRY_VERSION {
            STREAM_PREFIX_BYTES
        } else {
            SPLIT_STREAM_PREFIX_BYTES
        };
        if stream_count > MAX_AURA_PLAN_V2_STREAMS
            || decode_count > MAX_AURA_PLAN_V2_STREAMS
            || stream_count > reader.remaining() / stream_prefix
        {
            return Err(AuraError::InvalidValue("aura plan v2 stream count"));
        }
        let mut streams = Vec::new();
        streams
            .try_reserve_exact(stream_count)
            .map_err(|_| AuraError::InvalidValue("aura plan v2 allocation"))?;
        let mut total_dependencies = 0usize;
        for _ in 0..stream_count {
            let slot = reader.u16()?;
            let scope = FieldScope::from_code(reader.u8()?)?;
            let field_type = FieldType::from_code(reader.u8()?)?;
            let nullable = match reader.u8()? {
                0 => false,
                1 => true,
                _ => return Err(AuraError::InvalidValue("aura plan v2 nullable")),
            };
            let op = reader.u8()?;
            let dependency_count = reader.u16()? as usize;
            if reader.u32()? != 0 {
                return Err(AuraError::InvalidValue("aura plan v2 reserved"));
            }
            let physical_count = if registry_version == AURA_PLAN_V2_SPLIT_REGISTRY_VERSION {
                reader.u16()? as usize
            } else {
                0
            };
            total_dependencies = total_dependencies
                .checked_add(dependency_count)
                .filter(|value| *value <= MAX_AURA_PLAN_V2_DEPENDENCIES)
                .ok_or(AuraError::InvalidValue("aura plan v2 dependency count"))?;
            if dependency_count > reader.remaining() / 2 {
                return Err(AuraError::UnexpectedEof);
            }
            let mut dependencies = Vec::new();
            dependencies
                .try_reserve_exact(dependency_count)
                .map_err(|_| AuraError::InvalidValue("aura plan v2 allocation"))?;
            for _ in 0..dependency_count {
                dependencies.push(reader.u16()?);
            }
            if physical_count > reader.remaining() / 2
                || physical_count > MAX_AURA_PLAN_V2_STREAMS * 2
            {
                return Err(AuraError::InvalidValue(
                    "aura plan v2 physical stream count",
                ));
            }
            let mut physical_stream_ids = Vec::new();
            physical_stream_ids
                .try_reserve_exact(physical_count)
                .map_err(|_| AuraError::InvalidValue("aura plan v2 allocation"))?;
            for _ in 0..physical_count {
                physical_stream_ids.push(reader.u16()?);
            }
            streams.push(PlanV2StreamDescriptor {
                slot,
                scope,
                field_type,
                nullable,
                op,
                dependencies,
                physical_stream_ids,
            });
        }
        if decode_count > reader.remaining() / 2 {
            return Err(AuraError::UnexpectedEof);
        }
        let mut decode_order = Vec::new();
        decode_order
            .try_reserve_exact(decode_count)
            .map_err(|_| AuraError::InvalidValue("aura plan v2 allocation"))?;
        for _ in 0..decode_count {
            decode_order.push(reader.u16()?);
        }
        if reader.remaining() != 0 {
            return Err(AuraError::TrailingBytes(reader.remaining()));
        }
        let plan = Self {
            plan_version,
            registry_version,
            schema_id,
            schema_fingerprint,
            group_id,
            discriminator_slot,
            source_order,
            selection,
            authorized_relationship_bits,
            event_child_offsets_stream_id: (registry_version
                == AURA_PLAN_V2_SPLIT_REGISTRY_VERSION)
                .then_some(offsets_stream_raw),
            source_order_selector_stream_id: (registry_version
                == AURA_PLAN_V2_SPLIT_REGISTRY_VERSION)
                .then_some(selector_stream_raw),
            streams,
            decode_order,
        };
        plan.validate(schema)?;
        if plan.encode(schema)? != bytes {
            return Err(AuraError::InvalidValue("aura plan v2 noncanonical"));
        }
        Ok(plan)
    }

    pub fn hash(&self, schema: &SchemaDescriptor) -> Result<[u8; 32]> {
        let bytes = self.encode(schema)?;
        Ok(bytes[bytes.len() - HASH_BYTES..].try_into().unwrap())
    }
}

fn physical_stream_order(plan: &AuraPlanV2) -> Vec<usize> {
    let mut order = plan
        .streams
        .iter()
        .enumerate()
        .filter_map(|(index, stream)| (stream.scope == FieldScope::Event).then_some(index))
        .collect::<Vec<_>>();
    if let Some(index) = plan
        .streams
        .iter()
        .position(|stream| stream.slot == plan.discriminator_slot)
    {
        order.push(index);
    }
    order.extend(
        plan.streams
            .iter()
            .enumerate()
            .filter_map(|(index, stream)| {
                (stream.scope == FieldScope::Repeated && stream.slot != plan.discriminator_slot)
                    .then_some(index)
            }),
    );
    order
}

fn validate_acyclic(streams: &[PlanV2StreamDescriptor], field_count: usize) -> Result<()> {
    let mut outgoing = Vec::new();
    outgoing
        .try_reserve_exact(field_count)
        .map_err(|_| AuraError::InvalidValue("aura plan v2 allocation"))?;
    outgoing.resize_with(field_count, Vec::new);
    let mut indegree = Vec::new();
    indegree
        .try_reserve_exact(field_count)
        .map_err(|_| AuraError::InvalidValue("aura plan v2 allocation"))?;
    indegree.resize(field_count, 0usize);
    let mut dependency_total = 0usize;
    for stream in streams {
        let target = usize::from(stream.slot);
        dependency_total = dependency_total
            .checked_add(stream.dependencies.len())
            .filter(|value| *value <= MAX_AURA_PLAN_V2_DEPENDENCIES)
            .ok_or(AuraError::InvalidValue("aura plan v2 dependency count"))?;
        indegree[target] = stream.dependencies.len();
        for dependency in &stream.dependencies {
            let source = usize::from(*dependency);
            let edges = outgoing
                .get_mut(source)
                .ok_or(AuraError::InvalidValue("aura plan v2 dependency slot"))?;
            edges
                .try_reserve(1)
                .map_err(|_| AuraError::InvalidValue("aura plan v2 allocation"))?;
            edges.push(target);
        }
    }
    let mut ready = Vec::new();
    ready
        .try_reserve_exact(field_count)
        .map_err(|_| AuraError::InvalidValue("aura plan v2 allocation"))?;
    for (slot, degree) in indegree.iter().copied().enumerate() {
        if degree == 0 {
            ready.push(slot);
        }
    }
    let mut visited = 0usize;
    while let Some(source) = ready.pop() {
        visited += 1;
        for target in &outgoing[source] {
            indegree[*target] = indegree[*target]
                .checked_sub(1)
                .ok_or(AuraError::InvalidValue("aura plan v2 dependency graph"))?;
            if indegree[*target] == 0 {
                ready.push(*target);
            }
        }
    }
    if visited != field_count {
        return Err(AuraError::InvalidValue("aura plan v2 dependency cycle"));
    }
    Ok(())
}

fn plan_hash_bytes(bytes: &[u8], registry_version: u16) -> Result<[u8; 32]> {
    let len =
        u64::try_from(bytes.len()).map_err(|_| AuraError::InvalidValue("aura plan v2 length"))?;
    let mut hasher = Sha256::new();
    hasher.update(if registry_version == AURA_PLAN_V2_SPLIT_REGISTRY_VERSION {
        SPLIT_PLAN_HASH_DOMAIN
    } else {
        PLAN_HASH_DOMAIN
    });
    hasher.update(len.to_le_bytes());
    hasher.update(bytes);
    Ok(hasher.finalize().into())
}

fn put_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn put_u16_len(bytes: &mut Vec<u8>, value: usize, name: &'static str) -> Result<()> {
    put_u16(
        bytes,
        u16::try_from(value).map_err(|_| AuraError::InvalidValue(name))?,
    );
    Ok(())
}

fn put_u32_len(bytes: &mut Vec<u8>, value: usize, name: &'static str) -> Result<()> {
    put_u32(
        bytes,
        u32::try_from(value).map_err(|_| AuraError::InvalidValue(name))?,
    );
    Ok(())
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    const fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or(AuraError::UnexpectedEof)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(AuraError::UnexpectedEof)?;
        self.offset = end;
        Ok(value)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn array32(&mut self) -> Result<[u8; 32]> {
        Ok(self.take(32)?.try_into().unwrap())
    }
}
