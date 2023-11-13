// Copyright (C) Hygon Info Technologies Ltd.
//
// SPDX-License-Identifier: Apache-2.0
//

use anyhow::{Context, Result};
use log::{debug, warn};
extern crate serde;
use self::serde::{Deserialize, Serialize};
use super::*;
use async_trait::async_trait;
use base64::Engine;
use codicon::Decoder;
use csv_rs::{
    api::guest::{AttestationReport, Body},
    certs::{ca, csv, Verifiable},
};
use serde_json::json;

#[derive(Serialize, Deserialize)]
struct CertificateChain {
    hsk: ca::Certificate,
    cek: csv::Certificate,
    pek: csv::Certificate,
}

#[derive(Serialize, Deserialize)]
struct CsvEvidence {
    attestation_report: String,
    cert_chain: CertificateChain,
}

pub const HRK: &[u8] = include_bytes!("hrk.cert");

#[derive(Debug, Default)]
pub struct CsvVerifier {}

#[async_trait]
impl Verifier for CsvVerifier {
    async fn evaluate(
        &self,
        evidence: &[u8],
        expected_report_data: &ReportData,
        expected_init_data_hash: &InitDataHash,
    ) -> Result<TeeEvidenceParsedClaim> {
        let tee_evidence =
            serde_json::from_slice::<CsvEvidence>(evidence).context("Deserialize Quote failed.")?;

        let attestation_report = base64::engine::general_purpose::STANDARD
            .decode(tee_evidence.attestation_report)
            .context("base64 decode attestation report")?;

        let attestation_report: AttestationReport = serde_json::from_slice(&attestation_report)
            .context("parse attestation report failed")?;
        verify_report_signature(&attestation_report, &tee_evidence.cert_chain)?;

        let report_raw = restore_attestation_report(attestation_report)?;

        if let ReportData::Value(expected_report_data) = expected_report_data {
            debug!("Check the binding of REPORT_DATA.");
            if *expected_report_data != report_raw.body.report_data {
                bail!("REPORT_DATA is different from that in CSV Quote");
            }
        }

        if let InitDataHash::Value(_) = expected_init_data_hash {
            warn!("CSV does not support init data hash mechanism. skip.");
        }

        parse_tee_evidence(&report_raw)
    }
}

fn verify_report_signature(
    attestation_report: &AttestationReport,
    cert_chain: &CertificateChain,
) -> Result<()> {
    // Verify certificate chain
    let hrk = ca::Certificate::decode(&mut &HRK[..], ())?;
    (&hrk, &hrk)
        .verify()
        .context("HRK cert Signature validation failed.")?;
    (&hrk, &cert_chain.hsk)
        .verify()
        .context("HSK cert Signature validation failed.")?;
    (&cert_chain.hsk, &cert_chain.cek)
        .verify()
        .context("CEK cert Signature validation failed.")?;
    (&cert_chain.cek, &cert_chain.pek)
        .verify()
        .context("PEK cert Signature validation failed.")?;

    // Verify the TEE Hardware signature.

    (&cert_chain.pek, attestation_report)
        .verify()
        .context("Attestation Report Signature validation failed.")?;

    Ok(())
}

fn xor_with_anonce(data: &mut [u8], anonce: &u32) {
    let mut anonce_array = [0u8; 4];
    anonce_array[..].copy_from_slice(&anonce.to_le_bytes());

    for (index, item) in data.iter_mut().enumerate() {
        *item ^= anonce_array[index % 4];
    }
}

fn restore_attestation_report(report: AttestationReport) -> Result<AttestationReport> {
    let body = &report.body;
    let mut user_pubkey_digest = body.user_pubkey_digest;
    xor_with_anonce(&mut user_pubkey_digest, &report.anonce);
    let mut vm_id = body.vm_id;
    xor_with_anonce(&mut vm_id, &report.anonce);
    let mut vm_version = body.vm_version;
    xor_with_anonce(&mut vm_version, &report.anonce);
    let mut report_data = body.report_data;
    xor_with_anonce(&mut report_data, &report.anonce);
    let mut mnonce = body.mnonce;
    xor_with_anonce(&mut mnonce, &report.anonce);
    let mut measure = body.measure;
    xor_with_anonce(&mut measure, &report.anonce);

    let policy = report.body.policy.xor(&report.anonce);

    Ok(AttestationReport {
        body: Body {
            user_pubkey_digest,
            vm_id,
            vm_version,
            report_data,
            mnonce,
            measure,
            policy,
        },
        ..report
    })
}

// Dump the CSV information from the report.
fn parse_tee_evidence(report: &AttestationReport) -> Result<TeeEvidenceParsedClaim> {
    let body = &report.body;
    let claims_map = json!({
        // policy fields
        "policy_nodbg": format!("{}",body.policy.nodbg()),
        "policy_noks": format!("{}", body.policy.noks()),
        "policy_es": format!("{}", body.policy.es()),
        "policy_nosend": format!("{}", body.policy.nosend()),
        "policy_domain": format!("{}", body.policy.domain()),
        "policy_csv": format!("{}", body.policy.csv()),
        "policy_csv3": format!("{}", body.policy.csv3()),
        "policy_asid_reuse": format!("{}", body.policy.asid_reuse()),
        "policy_hsk_version": format!("{}", body.policy.hsk_version()),
        "policy_cek_version": format!("{}", body.policy.cek_version()),
        "policy_api_major": format!("{}", body.policy.api_major()),
        "policy_api_minor": format!("{}", body.policy.api_minor()),

        // launch info inject with pdh and session data
        "user_pubkey_digest": format!("{}", base64::engine::general_purpose::STANDARD.encode(body.user_pubkey_digest)),
        "vm_id": format!("{}", base64::engine::general_purpose::STANDARD.encode(body.vm_id)),
        "vm_version": format!("{}", base64::engine::general_purpose::STANDARD.encode(body.vm_version)),

        // measurement
        "measurement": format!("{}", base64::engine::general_purpose::STANDARD.encode(body.measure)),
    });

    Ok(claims_map as TeeEvidenceParsedClaim)
}