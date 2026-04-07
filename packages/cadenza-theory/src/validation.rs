use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValidationLevel { Info, Warning, Error }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationWarning {
    pub level: ValidationLevel,
    pub message: String,
}
