use std::collections::HashSet;

use crate::graph::GraphBackend;

/// Extract gRPC service contracts from .proto files in the graph.
///
/// Queries for proto Service symbols (kind='Class') in .proto files
/// and their child Method symbols (RPC methods), producing one Contract
/// per RPC endpoint with kind=GrpcService.
pub fn extract_grpc_contracts(backend: &dyn GraphBackend) -> Vec<super::Contract> {
    // Find services in .proto files
    let query = "MATCH (s:Symbol) WHERE s.kind = 'Class' AND s.file ENDS WITH '.proto' RETURN s.name, s.file, s.id";
    let services = match backend.raw_query(query) {
        Ok(r) => r,
        Err(_) => return vec![],
    };

    let mut contracts = Vec::new();
    for svc_row in &services {
        if svc_row.len() < 3 {
            continue;
        }
        let svc_name = &svc_row[0];
        let svc_file = &svc_row[1];

        // Find RPC methods for this service
        let rpc_query = format!(
            "MATCH (s:Symbol) WHERE s.kind = 'Method' AND s.file = '{}' AND s.parent = '{}' RETURN s.name, s.id",
            svc_file.replace('\'', "\\'"),
            svc_name.replace('\'', "\\'"),
        );
        if let Ok(rpcs) = backend.raw_query(&rpc_query) {
            for rpc in &rpcs {
                if rpc.is_empty() {
                    continue;
                }
                contracts.push(super::Contract {
                    kind: super::ContractKind::GrpcService,
                    service: svc_name.clone(),
                    method: "GRPC".to_string(),
                    path: format!("/{}/{}", svc_name, rpc[0]),
                    symbol_id: rpc.get(1).cloned().unwrap_or_default(),
                    file: svc_file.clone(),
                });
            }
        }
    }
    contracts
}

/// Detect gRPC client usage patterns in source files.
///
/// Looks for symbols referencing gRPC service stubs/clients:
///   - `ServiceNameStub`
///   - `ServiceNameClient`
///   - `ServiceNameGrpc`
///   - `service_name_pb2_grpc` (Python pattern)
pub fn detect_grpc_clients(
    backend: &dyn GraphBackend,
    contracts: &[super::Contract],
) -> Vec<super::CrossServiceDep> {
    if contracts.is_empty() {
        return vec![];
    }

    // Build unique service names from gRPC contracts
    let svc_names: HashSet<&str> = contracts
        .iter()
        .filter(|c| c.kind == super::ContractKind::GrpcService)
        .map(|c| c.service.as_str())
        .collect();

    let mut deps = Vec::new();

    for svc_name in &svc_names {
        // Search for symbols referencing this service (Stub, Client patterns)
        let patterns = [
            format!("{}Stub", svc_name),
            format!("{}Client", svc_name),
            format!("{}Grpc", svc_name),
            format!("{}_pb2_grpc", to_snake_case(svc_name)),
        ];

        for pattern in &patterns {
            let query = format!(
                "MATCH (s:Symbol) WHERE s.name CONTAINS '{}' AND NOT s.file ENDS WITH '.proto' RETURN s.name, s.file, s.id",
                pattern.replace('\'', "\\'"),
            );
            if let Ok(rows) = backend.raw_query(&query) {
                for row in &rows {
                    if row.len() < 2 {
                        continue;
                    }
                    deps.push(super::CrossServiceDep {
                        caller_service: String::new(), // filled by caller
                        caller_file: row[1].clone(),
                        caller_symbol: row.get(2).cloned().unwrap_or_default(),
                        target_service: svc_name.to_string(),
                        target_method: "GRPC".to_string(),
                        target_path: format!("/{}", svc_name),
                        url_found: format!("grpc://{}", svc_name),
                    });
                }
            }
        }
    }
    deps
}

/// Convert PascalCase/camelCase to snake_case for Python gRPC pattern matching.
fn to_snake_case(s: &str) -> String {
    let mut result = String::new();
    for (i, ch) in s.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            result.push('_');
        }
        result.push(ch.to_lowercase().next().unwrap_or(ch));
    }
    result
}
