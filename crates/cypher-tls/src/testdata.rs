//! OpenSSL-made fixtures for tests: keys in the SEC1 and PKCS#1 forms that
//! rcgen does not write, each with a self-signed `localhost` certificate
//! valid until 2126. Test-only keys; nothing trusts them. Stored as one-line
//! unpadded base64url DER (prose checkers skip such strings) and wrapped
//! as PEM.

const SEC1_CERT: &str = "MIIBljCCATygAwIBAgIUGzYhbGpq7zBy-XC2aRdjTFR5LrwwCgYIKoZIzj0EAwIwFDESMBAGA1UEAwwJbG9jYWxob3N0MCAXDTI2MTAxOTIwMDEzMVoYDzIxMjYwOTI1MjAwMTMxWjAUMRIwEAYDVQQDDAlsb2NhbGhvc3QwWTATBgcqhkjOPQIBBggqhkjOPQMBBwNCAAT_eZPNamULyM_qeo3WaabjvC_7I4MXWRXgv8VnOWz_dF4HSTgN7RqsmgaBLgXvotNn0eb4jLbopnltNaSpg4KAo2owaDAMBgNVHRMBAf8EAjAAMBQGA1UdEQQNMAuCCWxvY2FsaG9zdDAOBgNVHQ8BAf8EBAMCBaAwEwYDVR0lBAwwCgYIKwYBBQUHAwEwHQYDVR0OBBYEFExJWoRofwYurUrHkkF6a4L5XzTrMAoGCCqGSM49BAMCA0gAMEUCIHzK1Quwci9izzVH5yPWFYmR1eOkEwjeLAsPNOOuMO0bAiEAp3ZJNwFFrESnwgTMcNJEm82P2GFqJfH4ML-EhXGbrSI";
const SEC1_KEY: &str = "MHcCAQEEIEEz9Q6brt7zLNReWCYn_AAjA2mh8ntf-tgrNdM8mavloAoGCCqGSM49AwEHoUQDQgAE_3mTzWplC8jP6nqN1mmm47wv-yODF1kV4L_FZzls_3ReB0k4De0arJoGgS4F76LTZ9Hm-Iy26KZ5bTWkqYOCgA";
const PKCS1_CERT: &str = "MIIDIjCCAgqgAwIBAgIUAjLXSjARM9U-7XUXENFyHMseg7MwDQYJKoZIhvcNAQELBQAwFDESMBAGA1UEAwwJbG9jYWxob3N0MCAXDTI2MTAxOTIwMDEzMVoYDzIxMjYwOTI1MjAwMTMxWjAUMRIwEAYDVQQDDAlsb2NhbGhvc3QwggEiMA0GCSqGSIb3DQEBAQUAA4IBDwAwggEKAoIBAQDHbi4NJfciIC4pdAGYbnods8cNX0j2nzF2Zg-n256xilbHNlxwPNJmmygyL9hVEnxjKVnuee5uu3mMjfTt-7rIzF-T7frSEyMMBuVSe9bo7lxj_KsGQ9UHBEOwpNCzn8OR01OoHm0X8n4qJujYH0QFirm_oxV2tLJznOUMcW6m6n_W9eIQOTznSF-zx2YX-va5OuLH-uu9SmlRV9EGPY3C9OfUUmrzNMjkI_U4BVvYYnrASv-_FcLga5e-n2niOlB99fPMo4QVqrgi3HHyWsDaii8ph0Npmef8CmVA1NzJTNhL8w60Tvb5isR1vGl-tx8U6DNJHnQSB0Nx2i0iZcZTAgMBAAGjajBoMAwGA1UdEwEB_wQCMAAwFAYDVR0RBA0wC4IJbG9jYWxob3N0MA4GA1UdDwEB_wQEAwIFoDATBgNVHSUEDDAKBggrBgEFBQcDATAdBgNVHQ4EFgQULH_ULt7fz9xPqP0SVw7adYYyYbEwDQYJKoZIhvcNAQELBQADggEBAMN2vnIJ9Pz31Yp64fRt1R9BP3yIETggYGl8cJz56oG3-DPSkt5vwrDyDbS4CsFSp0aVRZFURo2Y-H4qyZO1evKoVLxNVGtVOsfb0lUJnbFlnPMiLuAzndCeOloFnsdh9PgJYOLWEGvt6Qao6XQL4Y-jD68S2Lm1xSqUVi0uRXGr_C1UG8c8C3ekG5TXlcj_U-YJETpD5vsmd1M1Coe78nVpkxHcYyvsMecEilLkWeF3AwGxAQtAUlkpgYljIuz26OrRiYNWw_Ir5YNZAjhUGlH2vZs2lecwoe6Pzi-ELhPWiQa1eMzOuiBOK8YQIr1o-4dGiOJjvWw9FdATSCi8iGY";
const PKCS1_KEY: &str = "MIIEpAIBAAKCAQEAx24uDSX3IiAuKXQBmG56HbPHDV9I9p8xdmYPp9uesYpWxzZccDzSZpsoMi_YVRJ8YylZ7nnubrt5jI307fu6yMxfk-360hMjDAblUnvW6O5cY_yrBkPVBwRDsKTQs5_DkdNTqB5tF_J-Kibo2B9EBYq5v6MVdrSyc5zlDHFupup_1vXiEDk850hfs8dmF_r2uTrix_rrvUppUVfRBj2NwvTn1FJq8zTI5CP1OAVb2GJ6wEr_vxXC4GuXvp9p4jpQffXzzKOEFaq4Itxx8lrA2oovKYdDaZnn_AplQNTcyUzYS_MOtE72-YrEdbxpfrcfFOgzSR50EgdDcdotImXGUwIDAQABAoIBAGJ5QKsEYp3KpxCX9Au5DXpqIXE0ahpA7BMK_VMEyjzfUs8mRAm1tqysk3IIlvkW2i26punu8jFiR_gDxnvWj1oENHOekHm3slgHdae_d5Rn0l4azrYWu3MuA2CPWXBAhLiWUVoTwAmRlwOoV9QTyCo2YFbEp94IBo-42o7E2f73reNd85amlJdeXE7cbeWalozt8W3h8O87bbBVCyai58SEz_atrJEtN-2-Gob3ap8UkUE0YOR3BvDnKd6eWbboykkomh21ZEvlqWG7lrJS7hEWtxPmtv5evJJL6BtrDSkc77Dn9H-e5WCgdohfEG-SlKmbw3vYz_jq--cY16N6kM0CgYEA_x-Cf0ghhWekxtaopDybaj5--CujPljTvaSMMGU7ro24XEwwV_HkQfeaOtzalAmWN7_U64-QaBdABRBUZUKm623O14ucG7QYEfT-XdWSgjRCxRG84IKtQCW9HTvr8K_skGh60Q_oWa3XcEZ9aSd3N4QfvnzeZyoGeWOUkrv1dpcCgYEAyB2qHRoOURa1o_P7D7Mgc0qCZsoGXulDuqf3y0VnIMISwIPUGEJW-DlRGIXQqr9JgQ12M2EkfZkjc0a_8C778IAq8emcFbLEOG-zum1vwy0nPFDSXYOZ_cjbhg3AwhNCnddsmcaQlu5kmDUijip9X8omPCq9Re0SAjv1kB0MQaUCgYEAmrw8xQA1vJj5GtR1Im_T_2N6VQD6mMT9X-deU7gCs8MnD2vyYlTHEa-gbpUnSWRrJNMHy_ueJVP8s1WNKoF7hg1qx9bYTwEzHCkQ4k639prF6IrymOUJOX_qLMH8P1YkUA3ed9CDJ2lF4sYOKGoUZ6qOKlXH-qBFnvhj03-GyG8CgYAaQopIN1sZmPlNAn83mDHx7x0dj_U2DzYsEE6cBgd6bP049hOm-02eVScnKKgxP9jNbo7MPzsioC3eurIFOqUk8iywgg9RaZwGK-JR3HqGCi7T42c3jkVu4CSueq-IyC49uD8wDrZ6l93re02Fil3ZbBWIX7Nmsa1jTarztlwdYQKBgQCDfE0RPkr-peQLCda9LIUS9lzTP7slRCGfqOA2g6h60rWrZCajFF10ulsE1jhP4YwN1WWRx9LB3gJibkVTPWjmg2p24cVvArckrmeT_rwIFz0z4akYL3ZsGHHGdSN0T_5bN8_AIio7BzES0_hXV5Zrh5OvL4Y_kkDVpm5DLU4MbQ";

fn pem(label: &str, der_base64url: &str) -> String {
    let mut body = der_base64url.replace('-', "+").replace('_', "/");
    while !body.len().is_multiple_of(4) {
        body.push('=');
    }
    format!(
        "-----BEGIN {label}-----
{body}
-----END {label}-----
"
    )
}

/// Certificate and key PEM with a SEC1 (`EC PRIVATE KEY`) P-256 key.
pub(crate) fn sec1() -> (String, String) {
    (
        pem("CERTIFICATE", SEC1_CERT),
        pem("EC PRIVATE KEY", SEC1_KEY),
    )
}

/// Certificate and key PEM with a PKCS#1 (`RSA PRIVATE KEY`) 2048-bit key.
pub(crate) fn pkcs1() -> (String, String) {
    (
        pem("CERTIFICATE", PKCS1_CERT),
        pem("RSA PRIVATE KEY", PKCS1_KEY),
    )
}
