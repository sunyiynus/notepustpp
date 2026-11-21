use notepust_contracts::{JobId, MatchBatch, SearchMatch, SnapshotId};

/// UI-facing result budget.
#[derive(Clone, Copy, Debug)]
pub struct ResultBudget {
    pub max_matches: usize,
    pub max_bytes: usize,
    pub batch_matches: usize,
    pub batch_bytes: usize,
}

impl Default for ResultBudget {
    fn default() -> Self {
        Self {
            max_matches: 10_000,
            max_bytes: 32 * 1024 * 1024,
            batch_matches: 500,
            batch_bytes: 512 * 1024,
        }
    }
}

/// Bounded result accumulator. It never grows beyond the budget and makes
/// truncation explicit instead of pretending the collected count is a total.
pub struct ResultCollector {
    budget: ResultBudget,
    matches: Vec<SearchMatch>,
    approx_bytes: usize,
    truncated: bool,
}

impl ResultCollector {
    pub fn new(budget: ResultBudget) -> Self {
        Self {
            budget,
            matches: Vec::new(),
            approx_bytes: 0,
            truncated: false,
        }
    }

    /// Adds a match. Returns false when the budget has been reached; the
    /// caller should stop scanning and mark the job truncated.
    pub fn push(&mut self, item: SearchMatch) -> bool {
        if self.matches.len() >= self.budget.max_matches {
            self.truncated = true;
            return false;
        }
        let size = match_size(&item);
        if self.approx_bytes.saturating_add(size) > self.budget.max_bytes {
            self.truncated = true;
            return false;
        }
        self.approx_bytes += size;
        self.matches.push(item);
        true
    }

    pub fn matches(&self) -> &[SearchMatch] {
        &self.matches
    }

    pub fn truncated(&self) -> bool {
        self.truncated
    }

    pub fn approx_bytes(&self) -> usize {
        self.approx_bytes
    }
}

fn match_size(item: &SearchMatch) -> usize {
    // Two u64s, optional line, plus capture option/range pairs and Vec overhead.
    48 + item.captures.len() * 24
}

/// Splits matches into protocol batches that respect both count and serialized
/// size limits. Bytes are measured after serialization, so a batch never
/// exceeds the wire budget.
pub fn split_batches(
    job_id: JobId,
    snapshot_id: SnapshotId,
    matches: &[SearchMatch],
    bytes_scanned: u64,
    budget: ResultBudget,
) -> Vec<MatchBatch> {
    let mut batches = Vec::new();
    let mut sequence = 0u32;
    let mut index = 0usize;

    while index < matches.len() {
        let mut count = (matches.len() - index).min(budget.batch_matches);
        loop {
            let candidate = MatchBatch {
                job_id,
                snapshot_id,
                sequence,
                matches: matches[index..index + count].to_vec(),
                bytes_scanned,
            };
            let size = serde_json::to_vec(&candidate)
                .map(|json| json.len())
                .unwrap_or(usize::MAX);
            if size <= budget.batch_bytes || count == 1 {
                batches.push(candidate);
                index += count;
                sequence = sequence.wrapping_add(1);
                break;
            }
            count = count.div_ceil(2);
        }
    }
    batches
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(start: u64) -> SearchMatch {
        SearchMatch {
            start_utf8: start,
            end_utf8: start + 1,
            line: None,
            captures: vec![None],
        }
    }

    #[test]
    fn collector_stops_at_budget() {
        let mut collector = ResultCollector::new(ResultBudget {
            max_matches: 2,
            ..Default::default()
        });
        assert!(collector.push(hit(0)));
        assert!(collector.push(hit(1)));
        assert!(!collector.push(hit(2)));
        assert!(collector.truncated());
        assert_eq!(collector.matches().len(), 2);
    }

    #[test]
    fn batches_respect_count_and_bytes() {
        let matches: Vec<SearchMatch> = (0..1200).map(hit).collect();
        let budget = ResultBudget::default();
        let batches = split_batches(JobId::new(), SnapshotId::new(), &matches, 1234, budget);
        assert_eq!(
            batches
                .iter()
                .map(|batch| batch.matches.len())
                .sum::<usize>(),
            matches.len()
        );
        assert!(batches.iter().all(|batch| batch.matches.len() <= 500));
        assert!(batches.iter().all(|batch| {
            serde_json::to_vec(batch)
                .map(|json| json.len() <= budget.batch_bytes)
                .unwrap_or(false)
        }));
    }
}
