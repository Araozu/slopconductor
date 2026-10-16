/// Canonical identity and common directory for a Git checkout.
#[derive(Debug, Clone)]
pub struct Repository {
    pub path: String,
    pub common_dir: String,
}
