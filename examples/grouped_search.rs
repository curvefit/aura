//! Bounded synthetic diagnostic for the current grouped search policy.
//! Times cover complete candidate search/encoding; small cases are not throughput claims.
#[path = "../tests/support/grouped_fixture.rs"]
mod fixture;
use aura_codec::experimental::GroupedSearch;
use fixture::{batch, cross_only_schema, paired_batch, schema};

fn main() {
    use sha2::{Digest, Sha256};
    let mut results = Vec::new();
    for mode in ["mixed", "cross0", "cross1", "empty"] {
        let s = if mode.starts_with("cross") {
            cross_only_schema("frozen-selection")
        } else {
            schema("frozen-selection")
        };
        let batches = match mode {
            "mixed" => vec![batch(s.schema_id, 0, 64), batch(s.schema_id, 64, 64)],
            "cross0" => vec![paired_batch(s.schema_id, true)],
            "cross1" => vec![paired_batch(s.schema_id, false)],
            _ => vec![],
        };
        let mut times = Vec::new();
        let mut last = None;
        for i in 0..4 {
            let start = std::time::Instant::now();
            let artifact = GroupedSearch::CrossDomain
                .compile(&s, &batches, Default::default())
                .unwrap();
            let elapsed = start.elapsed().as_nanos();
            if i > 0 {
                times.push(elapsed);
            }
            last = Some(artifact);
        }
        let a = last.unwrap();
        results.push(serde_json::json!({"case":mode,"bytes":a.bytes.len(),"sha256":format!("{:x}",Sha256::digest(&a.bytes)),"times_ns":times,"candidates":a.inspection.candidates.iter().map(|r| serde_json::json!({"id":r.candidate_id,"bytes":r.complete_bytes,"selected":r.selected,"applicable":r.applicable,"authorized":r.authorized})).collect::<Vec<_>>()}));
    }
    println!("{}", serde_json::to_string_pretty(&results).unwrap());
}
