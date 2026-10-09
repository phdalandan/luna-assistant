use sysinfo::{MemoryRefreshKind, RefreshKind, System};

use super::catalog::CatalogModel;

/// Compute buffers and runtime overhead on top of weights and KV cache.
const OVERHEAD: u64 = 768 * 1024 * 1024;

/// Approximate memory needed to run `model` with `context_length` tokens.
pub fn required(model: &CatalogModel, context_length: u32) -> u64 {
    model.size + model.kv_bytes_per_token * u64::from(context_length) + OVERHEAD
}

pub fn available() -> u64 {
    let refresh = RefreshKind::nothing().with_memory(MemoryRefreshKind::nothing().with_ram());
    System::new_with_specifics(refresh).available_memory()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::catalog;

    #[test]
    fn larger_contexts_need_more_memory() {
        let qwen = catalog::find("qwen3-8b").unwrap();
        let gemma = catalog::find("gemma-3-12b").unwrap();
        assert!(required(qwen, 8192) > required(qwen, 4096));
        assert!(required(gemma, 4096) > required(qwen, 4096));
        // Roughly 6.4 GB for Qwen3 8B at the default context.
        assert_eq!(required(qwen, 4096) / 100_000_000, 64);
    }
}
