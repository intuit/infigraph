mod registry;

pub use registry::LanguageRegistry;

use anyhow::Result;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tree_sitter::{Language, Query};

use crate::model::{Relation, Symbol};

/// Version of the Rust-side extraction logic, mixed into every language pack's
/// fingerprint and therefore into every `Module.content_hash`.
///
/// Bump this when a change to `extract/` alters the graph produced from source
/// that hasn't itself changed — a symbol-ID scheme fix, a new derived edge, a
/// different span convention. Doing so re-extracts every file on the next
/// `infigraph index` instead of leaving existing indexes on the old behavior.
///
/// Query-file and grammar changes are detected automatically and need no bump.
pub const EXTRACTOR_SCHEMA_VERSION: u32 = 1;

/// Combine `parts` into one fingerprint, length-prefixing each so that different
/// splits can't collide (`"ab" + "c"` must not fingerprint the same as
/// `"a" + "bc"`).
///
/// Public so that out-of-crate extractors can build a fingerprint for
/// `LanguagePack::with_fingerprint_part` using this same convention.
pub fn fingerprint_parts(parts: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    format!("{:x}", hasher.finalize())
}

/// Identify a compiled grammar closely enough to detect an upgrade.
///
/// tree-sitter exposes no grammar version, so this uses the structural counts it
/// does expose. `parse_state_count` in particular shifts on essentially any
/// change to the grammar's rules, which is what we need to catch.
fn grammar_fingerprint(grammar: &Language) -> String {
    format!(
        "{}:{}:{}:{}",
        grammar.abi_version(),
        grammar.node_kind_count(),
        grammar.field_count(),
        grammar.parse_state_count(),
    )
}

/// A custom edge type that a language pack can define beyond the standard
/// CALLS/IMPORTS/INHERITS model. Custom edges are populated during extraction
/// when capture groups matching `@{capture}.source` / `@{capture}.target`
/// are found in relations.scm.
#[derive(Debug, Clone, Deserialize)]
pub struct CustomEdgeDef {
    pub name: String,
    pub capture: String,
}

/// Trait for custom extraction backends (e.g., JVM grammar plugins).
pub trait CustomExtractor: Send + Sync {
    fn extract(
        &self,
        path: &str,
        source: &[u8],
        language: &str,
    ) -> Result<(Vec<Symbol>, Vec<Relation>)>;
}

/// Parser backend — tree-sitter or runtime-loaded custom extractor.
pub enum ParserBackend {
    TreeSitter {
        grammar: Language,
        entity_query: Query,
        relation_query: Box<Query>,
        /// Optional query for resolving a captured `@inherit.parent`/`@inherit.child`
        /// node down to its base identifier when it's a compound wrapper (generics,
        /// qualified/dotted names, member expressions). `None` for languages whose
        /// grammar can't produce such compound shapes in an inheritance position, or
        /// where a single fully-anchored pattern in `relation_query` already handles it.
        inherit_decompose_query: Option<Box<Query>>,
    },
    Custom(Box<dyn CustomExtractor>),
}

/// A language pack bundles a parser backend with file extension mappings.
pub struct LanguagePack {
    pub name: String,
    pub extensions: Vec<String>,
    pub backend: ParserBackend,
    pub custom_edges: Vec<CustomEdgeDef>,
    /// Digest of everything about this pack that affects what extraction
    /// produces: the query sources, the grammar, the custom edge definitions,
    /// and `EXTRACTOR_SCHEMA_VERSION`. Folded into `content_fingerprint` so an
    /// upgraded extractor invalidates the incremental cache for its own
    /// language and no other.
    fingerprint: String,
}

impl LanguagePack {
    /// Create a tree-sitter-backed language pack from a grammar and raw query strings.
    pub fn new(
        name: &str,
        extensions: Vec<&str>,
        grammar: Language,
        entity_query_src: &str,
        relation_query_src: &str,
    ) -> Result<Self> {
        let entity_query = Query::new(&grammar, entity_query_src)?;
        let relation_query = Box::new(Query::new(&grammar, relation_query_src)?);
        let fingerprint = fingerprint_parts(&[
            &EXTRACTOR_SCHEMA_VERSION.to_le_bytes(),
            name.as_bytes(),
            grammar_fingerprint(&grammar).as_bytes(),
            entity_query_src.as_bytes(),
            relation_query_src.as_bytes(),
        ]);
        Ok(Self {
            name: name.to_string(),
            extensions: extensions.into_iter().map(String::from).collect(),
            backend: ParserBackend::TreeSitter {
                grammar,
                entity_query,
                relation_query,
                inherit_decompose_query: None,
            },
            custom_edges: Vec::new(),
            fingerprint,
        })
    }

    /// Attach a decomposition query used to resolve compound `@inherit.parent`/
    /// `@inherit.child` captures (generics, qualified names, member expressions) down
    /// to their base identifier. Only meaningful for `ParserBackend::TreeSitter` packs;
    /// a no-op on `Custom` backends.
    pub fn with_inherit_decompose(mut self, query_src: &str) -> Result<Self> {
        let mut applied = false;
        if let ParserBackend::TreeSitter {
            grammar,
            inherit_decompose_query,
            ..
        } = &mut self.backend
        {
            *inherit_decompose_query = Some(Box::new(Query::new(grammar, query_src)?));
            applied = true;
        }
        if applied {
            self.fingerprint = fingerprint_parts(&[
                self.fingerprint.as_bytes(),
                b"inherit_decompose",
                query_src.as_bytes(),
            ]);
        }
        Ok(self)
    }

    /// Create a tree-sitter-backed language pack with custom edge definitions.
    pub fn new_with_custom_edges(
        name: &str,
        extensions: Vec<&str>,
        grammar: Language,
        entity_query_src: &str,
        relation_query_src: &str,
        custom_edges: Vec<CustomEdgeDef>,
    ) -> Result<Self> {
        let mut pack = Self::new(
            name,
            extensions,
            grammar,
            entity_query_src,
            relation_query_src,
        )?;
        let fingerprint = {
            let mut parts: Vec<&[u8]> = vec![pack.fingerprint.as_bytes(), b"custom_edges"];
            for edge in &custom_edges {
                parts.push(edge.name.as_bytes());
                parts.push(edge.capture.as_bytes());
            }
            fingerprint_parts(&parts)
        };
        pack.fingerprint = fingerprint;
        pack.custom_edges = custom_edges;
        Ok(pack)
    }

    /// Create a language pack with a custom extraction backend.
    ///
    /// The fingerprint can only cover this crate's schema version and the pack's
    /// identity — a runtime-loaded extractor's own behavior is opaque from here,
    /// so upgrading a grammar plugin in place does not by itself invalidate the
    /// cache for its files. Use `with_fingerprint_part` to mix in whatever the
    /// caller knows about the extractor's version.
    pub fn new_custom(
        name: &str,
        extensions: Vec<String>,
        extractor: Box<dyn CustomExtractor>,
    ) -> Self {
        let fingerprint = fingerprint_parts(&[
            &EXTRACTOR_SCHEMA_VERSION.to_le_bytes(),
            b"custom_backend",
            name.as_bytes(),
        ]);
        Self {
            name: name.to_string(),
            extensions,
            backend: ParserBackend::Custom(extractor),
            custom_edges: Vec::new(),
            fingerprint,
        }
    }

    /// Mix additional caller-known versioning into this pack's fingerprint —
    /// intended for custom backends whose extraction behavior this crate can't
    /// inspect (see `new_custom`). Changing the part re-extracts only this
    /// pack's files.
    pub fn with_fingerprint_part(mut self, part: &str) -> Self {
        self.fingerprint = fingerprint_parts(&[self.fingerprint.as_bytes(), part.as_bytes()]);
        self
    }

    /// Opaque digest of this pack's extraction behavior. Exposed for diagnostics;
    /// callers deciding whether a file needs re-extraction want
    /// `content_fingerprint` instead.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// The value stored as `Module.content_hash` and compared against on the
    /// next run to decide whether `source` can be skipped.
    ///
    /// It covers both halves of staleness: the file's contents *and* the
    /// extraction behavior that produced its rows. Every producer of a
    /// `content_hash` and every consumer deciding to skip must go through this
    /// one function, or unchanged files re-extract on every run.
    pub fn content_fingerprint(&self, source: &[u8]) -> String {
        fingerprint_parts(&[source, self.fingerprint.as_bytes()])
    }
}

impl std::fmt::Debug for LanguagePack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LanguagePack")
            .field("name", &self.name)
            .field("extensions", &self.extensions)
            .finish()
    }
}
