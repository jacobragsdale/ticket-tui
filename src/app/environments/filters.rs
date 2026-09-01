//! The grammar the environments board reads. An ordinary [`FilterSchema`], so
//! the search box and the chips work the way they do on every other tab;
//! `findings:` is what the `Findings` chip writes into the query.

use super::rows::ServiceRow;
use crate::filter::FilterSchema;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ServiceSchema;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ServiceField {
    Service,
    Namespace,
    /// Whether any environment is missing something this service asks for,
    /// which is the one question the board exists to answer.
    Findings,
}

impl FilterSchema for ServiceSchema {
    type Field = ServiceField;
    type Row = ServiceRow;

    fn all() -> &'static [Self::Field] {
        &[
            ServiceField::Service,
            ServiceField::Namespace,
            ServiceField::Findings,
        ]
    }

    fn bar() -> &'static [Self::Field] {
        &[ServiceField::Findings]
    }

    fn parse(name: &str) -> Option<Self::Field> {
        match name.to_ascii_lowercase().as_str() {
            "service" | "name" | "workload" => Some(ServiceField::Service),
            "ns" | "namespace" => Some(ServiceField::Namespace),
            "findings" | "missing" => Some(ServiceField::Findings),
            _ => None,
        }
    }

    fn key(field: Self::Field) -> &'static str {
        match field {
            ServiceField::Service => "service",
            ServiceField::Namespace => "ns",
            ServiceField::Findings => "findings",
        }
    }

    fn label(field: Self::Field) -> &'static str {
        match field {
            ServiceField::Service => "Service",
            ServiceField::Namespace => "Namespace",
            ServiceField::Findings => "Findings",
        }
    }

    /// `yes` and `no` both, the way the Key Vault tab's `enabled:` takes both
    /// spellings, so neither is a query that quietly matches nothing.
    fn values(field: Self::Field, row: &Self::Row) -> Vec<String> {
        match field {
            ServiceField::Service => vec![row.workload.clone()],
            ServiceField::Namespace => vec![row.namespace.clone()],
            ServiceField::Findings => {
                if row.findings() > 0 {
                    vec!["yes".to_owned(), "true".to_owned()]
                } else {
                    vec!["no".to_owned(), "false".to_owned()]
                }
            }
        }
    }
}
