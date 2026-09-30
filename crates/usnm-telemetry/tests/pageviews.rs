//! Connection strings for the page view uploader.

use usnm_telemetry::pageviews::parse_connection_string;

#[test]
fn connection_strings() {
    let (k, e) = parse_connection_string(
        "InstrumentationKey=abc;IngestionEndpoint=https://eastus2-3.in.applicationinsights.azure.com/;LiveEndpoint=https://x/",
    )
    .unwrap();
    assert_eq!(k, "abc");
    assert_eq!(e, "https://eastus2-3.in.applicationinsights.azure.com/");
    let (_, e) = parse_connection_string("instrumentationkey=abc").unwrap();
    assert_eq!(e, "https://dc.services.visualstudio.com/");
    assert!(parse_connection_string("IngestionEndpoint=https://x/").is_err());
    assert!(parse_connection_string("InstrumentationKey=a;IngestionEndpoint=ftp://x").is_err());
}
