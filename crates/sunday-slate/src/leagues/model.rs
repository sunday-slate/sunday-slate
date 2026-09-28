use time::PrimitiveDateTime;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct League {
    pub id: i64,
    pub name: String,
    pub created_at: PrimitiveDateTime,
    pub updated_at: PrimitiveDateTime,
}
