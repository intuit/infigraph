mod calls;
pub(crate) mod inherits;

pub use calls::*;

/// Statistics from call/inheritance resolution.
#[derive(Debug)]
pub struct ResolveStats {
    pub total_calls: usize,
    pub resolved: usize,
    pub unresolved: usize,
    pub learned_resolved: usize,
    pub inherits_resolved: usize,
}

impl std::fmt::Display for ResolveStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.learned_resolved > 0 {
            write!(
                f,
                "Call resolution: {} cross-file calls, {} resolved ({} from learned patterns), {} unresolved (builtins/externals)",
                self.total_calls, self.resolved, self.learned_resolved, self.unresolved
            )?;
        } else {
            write!(
                f,
                "Call resolution: {} cross-file calls, {} resolved, {} unresolved (builtins/externals)",
                self.total_calls, self.resolved, self.unresolved
            )?;
        }
        if self.inherits_resolved > 0 {
            write!(f, ", {} inheritance edges resolved", self.inherits_resolved)?;
        }
        Ok(())
    }
}

// Shared helpers used by both calls and inherits submodules.

pub(crate) fn shortest_id<'a, I, F>(iter: I, pred: F) -> Option<String>
where
    I: Iterator<Item = &'a (String, String, String)>,
    F: Fn(&(String, String, String)) -> bool,
{
    iter.filter(|t| pred(t))
        .min_by(|(a, _, _), (b, _, _)| a.len().cmp(&b.len()).then_with(|| a.cmp(b)))
        .map(|(id, _, _)| id.clone())
}

pub(crate) fn escape(s: &str) -> String {
    s.replace('\'', "\\'")
}
