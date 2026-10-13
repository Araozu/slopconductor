/// Provenance of a known total. Input and output counters are provider-reported.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub enum UsageSource {
    Reported,
    Derived,
}

/// Best-known provider-neutral token usage for a turn.
///
/// Missing counters remain unknown. Stream updates are cumulative snapshots,
/// not increments; explicit zero is a known value. Detailed usage counters and
/// per-counter completeness remain future additions.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq, Default)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub total_source: Option<UsageSource>,
}

impl Usage {
    /// Sum independent requests; absent counters remain unknown.
    pub fn accumulate(self, next: Self) -> Self {
        let sum = |a: Option<u64>, b: Option<u64>| a.zip(b).and_then(|(a, b)| a.checked_add(b));
        let reported = self.total_source == Some(UsageSource::Reported)
            && next.total_source == Some(UsageSource::Reported);
        Self::from_reported(
            sum(self.input_tokens, next.input_tokens),
            sum(self.output_tokens, next.output_tokens),
            if reported {
                sum(self.total_tokens, next.total_tokens)
            } else {
                None
            },
        )
    }

    #[must_use]
    pub fn from_reported(input: Option<u64>, output: Option<u64>, total: Option<u64>) -> Self {
        let derived = input.zip(output).and_then(|(i, o)| i.checked_add(o));
        Self {
            input_tokens: input,
            output_tokens: output,
            total_tokens: total.or(derived),
            total_source: if total.is_some() {
                Some(UsageSource::Reported)
            } else {
                derived.map(|_| UsageSource::Derived)
            },
        }
    }

    /// Merge a cumulative provider snapshot without dropping absent fields or
    /// double-counting repeated updates. Recompute derived totals after changes.
    pub fn merge(&mut self, seen: Self) {
        self.input_tokens = seen.input_tokens.or(self.input_tokens);
        self.output_tokens = seen.output_tokens.or(self.output_tokens);
        if seen.total_source == Some(UsageSource::Reported) {
            self.total_tokens = seen.total_tokens;
            self.total_source = seen.total_source;
        } else if seen.input_tokens.is_some() || seen.output_tokens.is_some() {
            let updated = Self::from_reported(self.input_tokens, self.output_tokens, None);
            self.total_tokens = updated.total_tokens;
            self.total_source = updated.total_source;
        }
    }
}
