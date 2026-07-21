//! Louvain community detection for discovering functional modules in the code graph.
//!
//! Builds an undirected weighted graph from CALLS edges, then runs single-level
//! Louvain modularity optimization. Results are stored as Cluster nodes and
//! MEMBER_OF edges in the graph.

use std::collections::HashMap;

use anyhow::{Context, Result};

use crate::graph::GraphBackend;

/// Statistics returned after clustering.
#[derive(Debug)]
pub struct ClusterStats {
    /// Total number of clusters discovered.
    pub num_clusters: usize,
    /// Size of each cluster (number of symbols).
    pub cluster_sizes: Vec<usize>,
    /// Final modularity score.
    pub modularity: f64,
}

impl std::fmt::Display for ClusterStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Cluster Statistics:")?;
        writeln!(f, "  Clusters:    {}", self.num_clusters)?;
        writeln!(f, "  Modularity:  {:.4}", self.modularity)?;

        let mut sorted_sizes = self.cluster_sizes.clone();
        sorted_sizes.sort_unstable_by(|a, b| b.cmp(a));
        let top: Vec<_> = sorted_sizes.iter().take(10).collect();
        write!(f, "  Top sizes:   ")?;
        for (i, size) in top.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{}", size)?;
        }
        if sorted_sizes.len() > 10 {
            write!(f, " ... ({} more)", sorted_sizes.len() - 10)?;
        }
        writeln!(f)
    }
}

/// Run Louvain community detection on the code graph and store results.
///
/// 1. Queries all CALLS edges to build an undirected adjacency list.
/// 2. Runs single-level Louvain (iterative modularity optimization).
/// 3. Creates Cluster nodes and MEMBER_OF edges in the graph.
pub fn detect_clusters(backend: &dyn GraphBackend) -> Result<ClusterStats> {
    // Step 1: Fetch all CALLS edges as (source_id, target_id) pairs.
    let edge_rows = backend.raw_query("MATCH (a:Symbol)-[:CALLS]->(b:Symbol) RETURN a.id, b.id")?;

    // Build node index: map symbol ID -> dense integer index.
    let mut id_to_idx: HashMap<String, usize> = HashMap::new();
    let mut idx_to_id: Vec<String> = Vec::new();

    for row in &edge_rows {
        for col in row {
            if !id_to_idx.contains_key(col) {
                let idx = idx_to_id.len();
                id_to_idx.insert(col.clone(), idx);
                idx_to_id.push(col.clone());
            }
        }
    }

    // Also include isolated symbols (no CALLS edges) so they appear in their own clusters.
    let all_symbols = backend.raw_query(
        "MATCH (s:Symbol) WHERE s.kind IN ['Function', 'Method', 'Class'] RETURN s.id",
    )?;
    for row in &all_symbols {
        if let Some(id) = row.first() {
            if !id_to_idx.contains_key(id) {
                let idx = idx_to_id.len();
                id_to_idx.insert(id.clone(), idx);
                idx_to_id.push(id.clone());
            }
        }
    }

    let n = idx_to_id.len();
    if n == 0 {
        return Ok(ClusterStats {
            num_clusters: 0,
            cluster_sizes: vec![],
            modularity: 0.0,
        });
    }

    // Build undirected weighted adjacency: adj[node] = Vec<(neighbor, weight)>.
    // For an undirected graph, each CALLS edge contributes weight 1 in both directions.
    // Multiple edges between the same pair accumulate weight.
    let mut edge_weight: HashMap<(usize, usize), f64> = HashMap::new();
    for row in &edge_rows {
        let a = id_to_idx[&row[0]];
        let b = id_to_idx[&row[1]];
        if a == b {
            continue; // skip self-loops for community detection
        }
        let key_ab = (a.min(b), a.max(b));
        *edge_weight.entry(key_ab).or_insert(0.0) += 1.0;
    }

    // Build adjacency list from the edge weights.
    let mut adj: Vec<Vec<(usize, f64)>> = vec![vec![]; n];
    let mut total_weight = 0.0;
    for (&(a, b), &w) in &edge_weight {
        adj[a].push((b, w));
        adj[b].push((a, w));
        total_weight += w; // each undirected edge counted once
    }

    if total_weight == 0.0 {
        // No edges: each node is its own cluster.
        let assignments: Vec<usize> = (0..n).collect();
        let stats = store_clusters(backend, &idx_to_id, &assignments, 0.0)?;
        return Ok(stats);
    }

    // m = sum of all edge weights (undirected). In modularity formula, 2m is the denominator.
    let m = total_weight;
    let two_m = 2.0 * m;

    // Degree of each node (sum of weights of incident edges).
    let mut degree: Vec<f64> = vec![0.0; n];
    for (&(a, b), &w) in &edge_weight {
        degree[a] += w;
        degree[b] += w;
    }

    // Step 2: Louvain single-level optimization.
    // community[i] = community label for node i
    let mut community: Vec<usize> = (0..n).collect();
    // Sum of degrees in each community.
    let mut sigma_tot: Vec<f64> = degree.clone();

    let max_iterations = 20;
    for _iter in 0..max_iterations {
        let mut improved = false;

        for node in 0..n {
            let node_comm = community[node];
            let k_i = degree[node];

            // Compute sum of weights from node to each neighboring community.
            let mut comm_weights: HashMap<usize, f64> = HashMap::new();
            for &(neighbor, w) in &adj[node] {
                let nc = community[neighbor];
                *comm_weights.entry(nc).or_insert(0.0) += w;
            }

            // Compute weight from node to its own community.
            let k_i_in = comm_weights.get(&node_comm).copied().unwrap_or(0.0);

            // Remove node from its community.
            sigma_tot[node_comm] -= k_i;

            let mut best_comm = node_comm;
            let mut best_delta = 0.0;

            for (&cand_comm, &k_i_cand) in &comm_weights {
                // Delta modularity for moving node to cand_comm:
                // delta_Q = [k_i_cand / m - sigma_tot[cand_comm] * k_i / (2 * m^2)]
                // compared to keeping node in its own singleton:
                // We use the standard Louvain delta formula:
                // delta_Q = (k_i_cand - k_i_in) / m
                //         - k_i * (sigma_tot[cand_comm] - sigma_tot[node_comm]) / (2 * m * m)
                // But since we already removed node from node_comm, sigma_tot[node_comm] is updated.
                // Simplified formula (after removing from current):
                // gain = k_i_cand / m - sigma_tot[cand_comm] * k_i / (two_m * m)
                // loss = k_i_in / m - sigma_tot[node_comm] * k_i / (two_m * m)
                // delta = gain - loss
                let gain = k_i_cand / m - sigma_tot[cand_comm] * k_i / (two_m * m);
                let loss = k_i_in / m - sigma_tot[node_comm] * k_i / (two_m * m);
                let delta = gain - loss;

                if delta > best_delta {
                    best_delta = delta;
                    best_comm = cand_comm;
                }
            }

            // Move node to best community.
            community[node] = best_comm;
            sigma_tot[best_comm] += k_i;

            if best_comm != node_comm {
                improved = true;
            }
        }

        if !improved {
            break;
        }
    }

    // Compute final modularity.
    let modularity = compute_modularity(&community, &edge_weight, &degree, m);

    // Step 3: Store results in the graph.
    let stats = store_clusters(backend, &idx_to_id, &community, modularity)?;
    Ok(stats)
}

/// Compute modularity Q for the given partition.
fn compute_modularity(
    community: &[usize],
    edge_weight: &HashMap<(usize, usize), f64>,
    degree: &[f64],
    m: f64,
) -> f64 {
    if m == 0.0 {
        return 0.0;
    }
    let two_m = 2.0 * m;
    let mut q = 0.0;

    for (&(a, b), &w) in edge_weight {
        if community[a] == community[b] {
            q += w - degree[a] * degree[b] / two_m;
        }
    }

    q / m
}

/// Store cluster results into the graph: create Cluster nodes and MEMBER_OF edges.
/// Clears any existing Cluster/MEMBER_OF data first.
fn store_clusters(
    backend: &dyn GraphBackend,
    idx_to_id: &[String],
    community: &[usize],
    modularity: f64,
) -> Result<ClusterStats> {
    let _ = backend.raw_query("MATCH (s:Symbol)-[r:MEMBER_OF]->(c:Cluster) DELETE r");
    let _ = backend.raw_query("MATCH (c:Cluster) DELETE c");

    // Build community -> members map, renumbering communities to be contiguous.
    let mut comm_members: HashMap<usize, Vec<usize>> = HashMap::new();
    for (node, &comm) in community.iter().enumerate() {
        comm_members.entry(comm).or_default().push(node);
    }

    let mut cluster_sizes = Vec::new();

    for (cluster_idx, members) in comm_members.values().enumerate() {
        let cluster_id = format!("cluster_{}", cluster_idx);
        let cluster_name = format!("Cluster {}", cluster_idx);

        // Gather file names for description.
        let mut files: Vec<&str> = Vec::new();
        for &node in members {
            let sym_id = &idx_to_id[node];
            // Extract file part from symbol ID (format: "file::name").
            if let Some((file, _)) = sym_id.rsplit_once("::") {
                if !files.contains(&file) {
                    files.push(file);
                }
            }
        }
        files.truncate(5);
        let description = format!(
            "{} symbols across files: {}",
            members.len(),
            files.join(", ")
        );

        // Create cluster node.
        let create_cluster = format!(
            "CREATE (c:Cluster {{id: '{}', name: '{}', description: '{}'}})",
            escape(&cluster_id),
            escape(&cluster_name),
            escape(&description),
        );
        backend
            .raw_query(&create_cluster)
            .with_context(|| format!("failed to create cluster node: {}", cluster_id))?;

        // Create MEMBER_OF edges.
        for &node in members {
            let sym_id = &idx_to_id[node];
            let create_edge = format!(
                "MATCH (s:Symbol), (c:Cluster) WHERE s.id = '{}' AND c.id = '{}' CREATE (s)-[:MEMBER_OF]->(c)",
                escape(sym_id),
                escape(&cluster_id),
            );
            let _ = backend.raw_query(&create_edge);
        }

        cluster_sizes.push(members.len());
    }

    Ok(ClusterStats {
        num_clusters: cluster_sizes.len(),
        cluster_sizes,
        modularity,
    })
}

fn escape(s: &str) -> String {
    s.replace('\'', "\\'")
}
