use std::path::PathBuf;

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct CatalogId(pub String);

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct SolutionId(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogProject {
    pub id: CatalogId,
    pub name: String,
    pub remote_url: String,
    pub default_branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Solution {
    pub id: SolutionId,
    pub name: String,
    pub root: PathBuf,
    pub members: Vec<SolutionMember>,
    pub last_opened_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SolutionMember {
    pub catalog_id: CatalogId,
    pub local_path: PathBuf,
}
