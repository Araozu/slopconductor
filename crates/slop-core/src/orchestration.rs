//! Pure rules for attempts and bounded, deterministic matrix expansion.

pub const MAX_MATRIX_MEMBERS: usize = 256;
pub const MAX_ATTEMPTS: u32 = 8;

pub fn retryable(status: &str) -> bool {
    matches!(
        status,
        "failed" | "interrupted" | "cancelled" | "incomplete"
    )
}

/// Prompt is the outer dimension, settings the fastest changing dimension.
pub fn matrix_indices(dimensions: [usize; 3]) -> Option<Vec<[usize; 3]>> {
    let count = dimensions
        .into_iter()
        .try_fold(1usize, usize::checked_mul)?;
    if count == 0 || count > MAX_MATRIX_MEMBERS {
        return None;
    }
    let mut indices = Vec::with_capacity(count);
    for prompt in 0..dimensions[0] {
        for model in 0..dimensions[1] {
            for settings in 0..dimensions[2] {
                indices.push([prompt, model, settings]);
            }
        }
    }
    Some(indices)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expansion_is_ordered_bounded_and_overflow_safe() {
        assert_eq!(
            matrix_indices([2, 2, 2]).unwrap(),
            vec![
                [0, 0, 0],
                [0, 0, 1],
                [0, 1, 0],
                [0, 1, 1],
                [1, 0, 0],
                [1, 0, 1],
                [1, 1, 0],
                [1, 1, 1],
            ]
        );
        assert!(matrix_indices([0, 2, 2]).is_none());
        assert!(matrix_indices([257, 1, 1]).is_none());
        assert!(matrix_indices([usize::MAX, 2, 2]).is_none());
        assert!(!retryable("completed"));
        assert!(!retryable("running"));
        assert!(retryable("interrupted"));
    }
}
