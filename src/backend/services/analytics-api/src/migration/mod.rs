//! Database migrations for the Analytics API service.

mod m20260414_000001_init;

use sea_orm_migration::prelude::*;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(m20260414_000001_init::Migration)]
    }
}
