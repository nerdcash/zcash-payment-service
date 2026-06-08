#[tokio::main]
async fn main() -> Result<(), zcash_payment_service::error::AppError> {
    zcash_payment_service::run().await
}
