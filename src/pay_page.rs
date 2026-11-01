//! The hosted pay page: a self-contained HTML page served at `GET /pay/{payment_id}`.
//!
//! It shows the amount, the ZIP-321 payment URI as a link and a QR code, and polls
//! `GET /payments/{payment_id}` to show live status. When the payment is seen and the
//! session has a `return_url`, it sends the payer back there.

use axum::{
    extract::{Path, State},
    http::header,
    response::{Html, IntoResponse, Response},
};
use qrcode::{render::svg, EcLevel, QrCode};

use crate::{
    error::AppError,
    service::{PaymentStatus, PaymentStatusResponse},
    AppState,
};

/// Content-Security-Policy for the pay page. The page loads nothing from elsewhere and may
/// only call back to this service for status.
pub const PAY_PAGE_CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; img-src data:; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

pub async fn pay_page(
    State(state): State<AppState>,
    Path(payment_id): Path<String>,
) -> Result<Response, AppError> {
    let status = state.service.payment_status(&payment_id)?;
    let html = render_pay_page(&status)?;
    Ok((
        [
            (header::CONTENT_SECURITY_POLICY, PAY_PAGE_CSP),
            (header::REFERRER_POLICY, "no-referrer"),
            (header::CACHE_CONTROL, "no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        Html(html),
    )
        .into_response())
}

/// Renders the QR code for `text` as an inline SVG document.
pub fn qr_svg(text: &str) -> Result<String, AppError> {
    let code = QrCode::with_error_correction_level(text.as_bytes(), EcLevel::M)
        .map_err(|error| AppError::InvalidRequest(format!("cannot encode QR code: {error}")))?;
    Ok(code
        .render::<svg::Color<'_>>()
        .min_dimensions(256, 256)
        .quiet_zone(true)
        .build())
}

pub fn render_pay_page(status: &PaymentStatusResponse) -> Result<String, AppError> {
    // A very long message can make the URI too big for a QR code. Still serve the page so the
    // payer can use the wallet link or copy the address.
    let qr = qr_svg(&status.qr_text).unwrap_or_else(|_| {
        r#"<p class="message">The payment request is too long for a QR code. Use the wallet link or copy the address below.</p>"#.to_string()
    });
    // A settled link offers nothing more to pay, so a reload never shows a new request.
    let controls = if matches!(status.status, PaymentStatus::Paid | PaymentStatus::Received) {
        String::new()
    } else {
        format!(
            r#"<div class="qr" aria-label="QR code for the payment request">{qr}</div>
<p><a class="button" href="{uri}">Open in wallet</a></p>"#,
            uri = escape_html(&status.qr_text),
        )
    };
    let title = status.label.as_deref().unwrap_or("Pay with Zcash");
    let remaining = status.amount.as_deref().and_then(|amount| {
        let received = crate::service::parse_zec_amount(&status.total_received).unwrap_or(0);
        crate::service::remaining_amount(Some(amount), received).filter(|rest| rest != amount)
    });
    let amount = match (status.amount.as_deref(), remaining) {
        // After a partial payment, lead with what is still owed so nobody resends the full sum.
        (Some(amount), Some(rest)) => format!(
            r#"<p class="amount"><span>{}</span> ZEC remaining</p><p class="message">of {} ZEC; {} ZEC received so far</p>"#,
            escape_html(&rest),
            escape_html(amount),
            escape_html(&status.total_received)
        ),
        (Some(amount), None) => format!(
            r#"<p class="amount"><span>{}</span> ZEC</p>"#,
            escape_html(amount)
        ),
        (None, _) => String::new(),
    };
    // Polling removes this once the link settles, so a stale "remaining" amount never lingers.
    let amount = if amount.is_empty() {
        amount
    } else {
        format!(r#"<div class="due">{amount}</div>"#)
    };
    let message = match status
        .message
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        Some(message) => format!(r#"<p class="message">{}</p>"#, escape_html(message)),
        None => String::new(),
    };
    // Values embedded in the script are JSON, with `<` escaped so the text can't end the
    // script element.
    let script_data = serde_json::to_string(&serde_json::json!({
        "paymentId": status.payment_id,
        "returnUrl": status.return_url,
        "qrText": status.qr_text,
        "status": status.status,
    }))?
    .replace('<', "\\u003c");

    Ok(format!(
        r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="robots" content="noindex">
<title>{title}</title>
<style>
body {{ font-family: system-ui, sans-serif; margin: 0; background: #f6f6f4; color: #1c1c1c; }}
main {{ max-width: 28rem; margin: 2rem auto; padding: 1.5rem; background: #fff; border-radius: 12px; box-shadow: 0 1px 4px rgba(0,0,0,.08); text-align: center; }}
h1 {{ font-size: 1.25rem; margin: 0 0 .5rem; }}
.amount {{ font-size: 1.75rem; font-weight: 600; margin: .5rem 0; }}
.qr svg {{ width: 16rem; height: 16rem; }}
.address {{ font-family: ui-monospace, monospace; font-size: .75rem; word-break: break-all; background: #f1f1ef; padding: .5rem; border-radius: 6px; }}
.button {{ display: inline-block; margin: .75rem 0; padding: .6rem 1.2rem; border-radius: 8px; background: #1c1c1c; color: #fff; text-decoration: none; }}
#status {{ margin-top: 1rem; font-weight: 600; }}
#status[data-status="paid"], #status[data-status="received"] {{ color: #19773b; }}
#status[data-status="partially_paid"] {{ color: #9a5b00; }}
</style>
</head>
<body>
<main>
<h1>{title}</h1>
{amount}
{message}
{controls}
<p class="address">{address}</p>
<p id="status" data-status="{status_key}" role="status" aria-live="polite">{status_text}</p>
<p id="total"></p>
</main>
<script>
(() => {{
  const data = {script_data};
  const labels = {{
    awaiting_payment: "Waiting for payment…",
    partially_paid: "Partial payment received. Please send the rest.",
    paid: "Payment received. Thank you!",
    received: "Payment received. Thank you!",
  }};
  const statusEl = document.getElementById("status");
  const totalEl = document.getElementById("total");
  const settled = (s) => s === "paid" || s === "received";
  // Whether the page currently lacks payment controls (rendered settled, or removed below).
  let controlsRemoved = settled(data.status);
  const done = (s) => (s === "paid" || s === "received") && !!data.returnUrl;
  const show = (s, total) => {{
    statusEl.dataset.status = s;
    statusEl.textContent = labels[s] || s;
    totalEl.textContent = total && Number(total) > 0 ? "Received so far: " + total + " ZEC" : "";
    if (s === "paid" || s === "received") {{
      document.querySelectorAll(".qr, a.button, .due").forEach((el) => el.remove());
      controlsRemoved = true;
    }}
  }};
  // Stop polling only when leaving for the return URL. Otherwise keep the shown state current,
  // since an unconfirmed receipt can still be dropped.
  const finish = () => {{
    setTimeout(() => {{ window.location.assign(data.returnUrl); }}, 1500);
  }};
  if (done(data.status)) {{ finish(); return; }}
  const poll = async () => {{
    try {{
      const response = await fetch("../payments/" + encodeURIComponent(data.paymentId), {{ headers: {{ accept: "application/json" }}, cache: "no-store" }});
      if (response.ok) {{
        const body = await response.json();
        show(body.status, body.total_received);
        if (done(body.status)) {{ finish(); return; }}
        // The request changes after a partial payment (it asks only for the rest), and a revoked
        // receipt can raise it again or bring back a request the page already removed. Reload
        // whenever there is still something to pay and the shown request is stale or missing.
        const unpaid = body.status === "partially_paid" || body.status === "awaiting_payment";
        if (unpaid && body.qr_text && (body.qr_text !== data.qrText || controlsRemoved)) {{ window.location.reload(); return; }}
      }}
    }} catch (e) {{ /* keep polling */ }}
    setTimeout(poll, 3000);
  }};
  setTimeout(poll, 3000);
}})();
</script>
</body>
</html>
"##,
        title = escape_html(title),
        amount = amount,
        message = message,
        controls = controls,
        address = escape_html(&status.address),
        status_key = status_key(status.status),
        status_text = status_text(status.status),
        script_data = script_data,
    ))
}

fn status_key(status: PaymentStatus) -> &'static str {
    match status {
        PaymentStatus::AwaitingPayment => "awaiting_payment",
        PaymentStatus::PartiallyPaid => "partially_paid",
        PaymentStatus::Paid => "paid",
        PaymentStatus::Received => "received",
    }
}

fn status_text(status: PaymentStatus) -> &'static str {
    match status {
        PaymentStatus::AwaitingPayment => "Waiting for payment…",
        PaymentStatus::PartiallyPaid => "Partial payment received. Please send the rest.",
        PaymentStatus::Paid | PaymentStatus::Received => "Payment received. Thank you!",
    }
}

pub fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            other => escaped.push(other),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::{escape_html, qr_svg};

    #[test]
    fn page_renders_without_qr_when_request_is_too_long() {
        let status = crate::service::PaymentStatusResponse {
            payment_id: "0".repeat(32),
            address: "utest1example".into(),
            qr_text: format!("zcash:utest1example?message={}", "a".repeat(8000)),
            pay_url: "/pay/x".into(),
            label: None,
            message: None,
            amount: None,
            total_received: "0.00000000".into(),
            state_version: 0,
            status: crate::service::PaymentStatus::AwaitingPayment,
            return_url: None,
            created_at: "2026-01-01T00:00:00Z".into(),
        };
        let html = super::render_pay_page(&status).unwrap();
        assert!(html.contains("too long for a QR code"));
        assert!(html.contains("utest1example"));
    }

    fn status_with(
        status: crate::service::PaymentStatus,
        total: &str,
    ) -> crate::service::PaymentStatusResponse {
        crate::service::PaymentStatusResponse {
            payment_id: "0".repeat(32),
            address: "utest1example".into(),
            qr_text: "zcash:utest1example?amount=0.30000000".into(),
            pay_url: "/pay/x".into(),
            label: None,
            message: None,
            amount: Some("0.5".into()),
            total_received: total.into(),
            state_version: 1,
            status,
            return_url: None,
            created_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn partial_payment_leads_with_the_remaining_amount() {
        let html = super::render_pay_page(&status_with(
            crate::service::PaymentStatus::PartiallyPaid,
            "0.20000000",
        ))
        .unwrap();
        assert!(html.contains("0.30000000</span> ZEC remaining"));
        assert!(html.contains("Open in wallet"));
    }

    #[test]
    fn settled_link_offers_no_payment_controls() {
        let html = super::render_pay_page(&status_with(
            crate::service::PaymentStatus::Paid,
            "0.50000000",
        ))
        .unwrap();
        assert!(!html.contains("Open in wallet"));
        assert!(!html.contains("<svg"));
    }

    #[test]
    fn page_reloads_for_a_revoked_payment() {
        let html = super::render_pay_page(&status_with(
            crate::service::PaymentStatus::Paid,
            "0.50000000",
        ))
        .unwrap();
        // Polling reloads for either unpaid status when the request changed or the controls are
        // gone, so a revoked receipt brings back a correct request.
        assert!(html.contains(r#"body.status === "awaiting_payment""#));
        assert!(html.contains("controlsRemoved"));
        // Settling also removes the amount still due.
        assert!(html.contains(r#".qr, a.button, .due"#));
    }

    #[test]
    fn escapes_html_special_characters() {
        assert_eq!(
            escape_html(r#"<a href="x">Tom & 'Jerry'</a>"#),
            "&lt;a href=&quot;x&quot;&gt;Tom &amp; &#39;Jerry&#39;&lt;/a&gt;"
        );
    }

    #[test]
    fn renders_qr_as_svg() {
        let svg = qr_svg("zcash:u1example?amount=0.5").unwrap();
        assert!(svg.contains("<svg"));
        assert!(svg.contains("</svg>"));
    }
}
