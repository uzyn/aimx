# Quarterly Operations Report — Q1 2026

This report consolidates operational performance across the engineering,
product, finance, and customer-success organisations for the first quarter
of 2026. Numbers are unaudited; the finance team's reconciled figures will
follow within ten business days.

> **Executive summary.** Q1 came in modestly ahead of plan on revenue, in
> line on gross margin, and meaningfully ahead on operating efficiency.
> Headcount grew 6% year-on-year — well below the 12% planned at the start
> of the year — driven by a deliberate slow-down in non-engineering hires.
> Customer churn ticked up by 40 bp to 2.1% but remains below the
> industry benchmark of ~3%. Three of the four major product launches
> planned for Q1 shipped on time; the fourth (Workflow Studio v3) slipped
> two weeks and is now scheduled for week 4 of Q2.

---

## 1. Financial Highlights

| Metric                       | Q1 2026   | Q1 2025   | YoY     | vs. plan |
|------------------------------|-----------|-----------|---------|----------|
| Revenue                      | 142.3M    | 118.7M    | +19.9%  | +1.6%    |
| Gross profit                 | 109.5M    | 91.4M     | +19.8%  | +0.0%    |
| Gross margin                 | 76.9%     | 77.0%     | -10 bp  | -10 bp   |
| Operating income             | 28.1M     | 19.6M     | +43.4%  | +14.3%   |
| Operating margin             | 19.8%     | 16.5%     | +330 bp | +250 bp  |
| Free cash flow               | 22.7M     | 14.2M     | +59.9%  | +21.0%   |
| Cash and equivalents         | 318.4M    | 261.0M    | +22.0%  | +5.4%    |

### 1.1 Revenue mix

- **Subscription revenue** grew 22% year-on-year to USD 128.0M, now 90% of total
  revenue (vs. 88% a year ago). New-logo ARR was USD 14.2M, up 38% YoY; net
  new ARR was USD 11.7M after USD 2.5M of gross churn.
- **Services revenue** grew 4% to USD 14.3M. The deliberate decision to
  package more of the implementation playbook into self-serve content has
  slowed services growth — exactly as intended.
- **Top-three customer concentration** fell from 18.4% to 14.1%, a
  healthier balance reflecting the broadening of the enterprise base.

### 1.2 Cost discipline

| Cost line              | Q1 2026 | Q1 2025 | YoY    | % of rev |
|------------------------|---------|---------|--------|----------|
| Cost of revenue        | 32.8M   | 27.3M   | +20.1% | 23.1%    |
| Sales and marketing    | 41.2M   | 38.7M   | +6.5%  | 28.9%    |
| Research and dev       | 27.4M   | 23.5M   | +16.6% | 19.3%    |
| General and admin      | 12.8M   | 10.3M   | +24.3% | 9.0%     |
| Total opex             | 81.4M   | 72.5M   | +12.3% | 57.2%    |

Sales and marketing efficiency improved materially: customer acquisition
cost (CAC) on new logos was USD 11.4k, down from USD 14.1k a year ago, driven
by a 24% lift in inbound conversion rates after the website re-launch in
February.

---

## 2. Engineering

### 2.1 Delivery cadence

The engineering organisation shipped **47 production releases** in Q1, up
from 38 in Q1 2025. Mean time to production for an approved PR fell from
2.6 days to 1.8 days, helped by the new merge-queue automation that
removes the manual rebase step for the long-tail of small PRs.

| Quarter | Releases | Mean TTP (days) | P95 TTP (days) |
|---------|----------|-----------------|----------------|
| Q1 2025 | 38       | 2.6             | 6.1            |
| Q2 2025 | 41       | 2.4             | 5.8            |
| Q3 2025 | 44       | 2.2             | 5.4            |
| Q4 2025 | 46       | 2.0             | 5.1            |
| Q1 2026 | 47       | 1.8             | 4.7            |

### 2.2 Reliability

Service-level objective attainment for the quarter:

| Service                | Target  | Actual  | Status |
|------------------------|---------|---------|--------|
| Public API (p99 lat.)  | < 350ms | 287ms   | Met    |
| Webhook delivery       | 99.9%   | 99.94%  | Met    |
| Auth (uptime)          | 99.99%  | 99.987% | Missed |
| Reporting (freshness)  | < 15min | 11min   | Met    |
| Dashboard (TTI)        | < 3.0s  | 2.4s    | Met    |

The single SLO miss — auth uptime — was driven by a 4-minute outage on
2026-02-19 caused by a cascading restart of the rate-limit cluster after
an inadvertent config push. The post-incident review identified two
process changes already in place: dual-control for prod config pushes,
and a pre-flight canary on the rate-limit nodes before a fleet-wide roll.

### 2.3 Tech-debt reduction

> The infrastructure team retired the v1 ingestion pipeline this quarter,
> consolidating onto the v2 columnar path. v1 had been receiving
> diminishing share of new traffic for three quarters; deprecation
> reduced our cloud spend by ~USD 240k per quarter and removed a known
> single-point-of-failure in the cross-region replication path.

Other notable retirements:

- **MongoDB to Postgres migration** for the audit-log service: complete.
  Storage cost down ~40%, query latency down ~3x at p95.
- **Legacy permissions cache**: replaced with the new RLS-aware
  authorization layer. Net reduction of ~14k lines of code.
- **Dual-write to the analytics warehouse**: removed; CDC pipeline is
  now the sole writer.

### 2.4 Hiring

Engineering added a net 11 people in Q1 (16 hires, 5 departures). Open
reqs at quarter-end: 8 (down from 14 at the start of the quarter).
Voluntary attrition annualised: 7.8%, well below the industry benchmark
of ~12-14%.

```text
Headcount by team (end of Q1):
  Platform .................... 22
  Application ................. 31
  Data and ML ................. 14
  Infrastructure ............... 9
  Security ..................... 6
  Engineering management ....... 7
  ----------------------------------
  Total ....................... 89
```

---

## 3. Product

### 3.1 Launches

Three of four planned major launches shipped in Q1:

1. **Universal Inbox** — beta opened to 800 customers; GA shipped 2026-03-12.
   Adoption is tracking ~2x the comparable launch a year ago, helped by
   in-app onboarding.
2. **Workflow Studio v3** — *slipped*; now scheduled for 2026-04-22.
   Slip was caused by a rewrite of the canvas renderer to support the
   new branching primitives. Quality bar held; we made the right call.
3. **Reporting v2** — GA shipped 2026-02-04; ~63% of eligible
   customers have migrated within the first eight weeks.
4. **Mobile push for SOC 2 alerts** — shipped 2026-03-28; uptake has
   been concentrated in the security-engineering buyer persona, as
   expected.

### 3.2 Adoption metrics

| Feature              | DAU/MAU | WAU/MAU | Stickiness |
|----------------------|---------|---------|------------|
| Universal Inbox      | 0.41    | 0.78    | High       |
| Reporting v2         | 0.32    | 0.71    | High       |
| Workflow automation  | 0.29    | 0.65    | Medium     |
| Knowledge graph      | 0.18    | 0.49    | Low        |
| API integrations     | 0.36    | 0.74    | High       |

### 3.3 Customer-requested top-10

The product council reviewed the top-10 customer-requested features at
the end of the quarter. Ranked by weighted ARR exposure:

1. **Cross-workspace search** — committed to Q2.
2. **SAML group-attribute mapping** — committed to Q2.
3. **Custom retention policies per data type** — Q3 candidate.
4. **Native Slack threading** — shipped (2026-03-19).
5. **Bulk archival via API** — Q3 candidate.
6. **CMK / BYOK for at-rest encryption** — Q3 commit.
7. **Read-only audit-log API** — committed to Q2.
8. **Webhook signing keys (per-endpoint rotation)** — Q3 candidate.
9. **Per-region data residency for EU** — H2 commit.
10. **Real-time presence in shared docs** — Q4 candidate.

---

## 4. Customer Success and Support

### 4.1 Net retention

Net dollar retention (NDR) for the quarter was **117%**, in line with
plan. Gross dollar retention was **94%**, a 50 bp improvement over Q1
2025. Cohort analysis shows the improvement is concentrated in the
mid-market segment, where the new account-health scoring is producing
higher renewal-touch coverage.

| Segment   | NDR   | GDR  | Logos | ARR (M) |
|-----------|-------|------|-------|---------|
| Enterprise| 124%  | 97%  | 142   | 78.4    |
| Mid-mkt   | 116%  | 95%  | 631   | 41.2    |
| SMB       | 102%  | 88%  | 4,210 | 22.7    |

### 4.2 Support

> Support handled 18,420 tickets in Q1, a 14% increase vs. Q1 2025 — in
> line with the customer-base growth. Median first response was 23
> minutes (target: under 30); median resolution was 7.4 hours (target: under 8).

| Tier      | Tickets | Median FR | Median TTR | CSAT |
|-----------|---------|-----------|------------|------|
| Critical  | 312     | 4 min     | 1.6 hr     | 4.6  |
| High      | 1,840   | 11 min    | 4.1 hr     | 4.5  |
| Standard  | 12,210  | 27 min    | 8.2 hr     | 4.4  |
| Low       | 4,058   | 1.4 hr    | 18.7 hr    | 4.3  |

CSAT (overall): **4.46** out of 5, up from 4.41 in Q4 2025.

---

## 5. Risk Register

### 5.1 Top risks

1. **Concentration risk on a single payment processor.** ~62% of
   payment volume flows through one PSP; a 12-week incident in 2025
   demonstrated the impact. Mitigation: integration with a second
   processor is engineering-complete and in finance-side UAT;
   targeted go-live mid-Q2.
2. **Regulatory exposure on cross-border data transfers.** A pending
   ruling in the EU could narrow the lawful basis for some transfers.
   Mitigation: per-region data residency commit (see 3.3 #9).
3. **Senior-engineer attrition concentrated in two teams.** Two staff
   engineers gave notice in Q1; both are in the same team. Mitigation:
   on-the-fly knowledge-share sessions and an active backfill on each
   role; succession plans documented for both.

### 5.2 Closed risks

- **Auth-cluster single-region exposure** — closed Q1 with the
  multi-region rollout.
- **No on-call escalation for Reporting** — closed Q1 with the
  reorg of the data team.

---

## 6. People

### 6.1 Headcount

| Org         | Start of Q1 | Hires | Leavers | End of Q1 |
|-------------|-------------|-------|---------|-----------|
| Engineering | 83          | 16    | 5       | 89        |
| Product     | 18          | 2     | 0       | 20        |
| Design      | 9           | 1     | 1       | 9         |
| GTM         | 41          | 4     | 3       | 42        |
| G and A     | 14          | 1     | 0       | 15        |
| **Total**   | **165**     | **24**| **9**   | **175**   |

Voluntary attrition annualised: **6.4%** company-wide.

### 6.2 Engagement

Pulse-survey engagement score: **8.1 / 10** (Q4 2025: 8.0). The largest
positive shift was in *clarity of strategy* (7.8 to 8.4), reflecting the
narrative work the leadership team did in late Q4. The largest negative
shift was in *cross-team collaboration* (7.6 to 7.2), which the COO is
addressing with a formal program-management function being stood up
in Q2.

---

## 7. Outlook

### 7.1 Q2 commitments

- **Workflow Studio v3** ships by end of week 4.
- **Cross-workspace search** ships by end of Q2.
- **Second payment-processor integration** GA by end of Q2.
- **Read-only audit-log API** GA by end of Q2.
- **SAML group-attribute mapping** ships in Q2.

### 7.2 Q2 financial guidance

| Metric              | Guide          | Notes |
|---------------------|----------------|-------|
| Revenue             | 152 to 154M    | Low end assumes Workflow Studio slips a third week. |
| Gross margin        | 76.5% to 77.5% |       |
| Operating margin    | 18.5% to 20.0% |       |
| Free cash flow      | 24 to 27M      |       |
| Net new ARR         | 13 to 15M      |       |

### 7.3 H2 themes

- **CMK / BYOK at-rest encryption** as a strategic enterprise unblocker.
- **Real-time collaboration primitives** in shared docs and dashboards.
- **EU data residency** as a regulatory hedge and an enterprise sales lever.
- **Platform observability** — bringing internal SLI / SLO tooling to
  parity with the public-facing reliability surface.

---

## 8. Appendix

### 8.1 Definitions

- **ARR (annualised recurring revenue)**: end-of-period MRR multiplied by 12,
  excluding one-time fees and services revenue.
- **Net dollar retention (NDR)**: ARR from a cohort at the end of the period
  divided by the ARR of that same cohort at the start of the period;
  includes expansion, contraction, and churn.
- **Gross dollar retention (GDR)**: same numerator excluding expansion;
  measures pure retention on the contracted base.
- **Mean time to production (TTP)**: median time from "PR approved" to
  "merged commit live in production".

### 8.2 Reading list

- Internal: *Reliability Review for Q1* — published 2026-04-09.
- Internal: *Capacity Plan H2 2026* — draft circulating with leadership.
- External: [State of DevOps 2025 - DORA report](https://www.example.com/dora-2025).
- External: [Why net retention beats new logos in this market](https://www.example.com/net-retention).

### 8.3 Methodology notes

Numbers in this report are derived from the standard reporting warehouse
as of the close of business on 2026-04-04. Any subsequent restatements
will be reflected in the Q2 report. Historical comparisons use the
restated 2025 figures published in the Q4 2025 reconciliation memo.

---

## 9. Operational Detail Index

Each operational metric below is captured for archive and for the
quarter-by-quarter trend file maintained by the program office. Numbers
are pulled from the standard warehouse and validated by the relevant
function lead before publication.

### 9.1 Engineering throughput trends

| Week | Releases | PRs merged | Mean LOC | Defects |
|------|----------|------------|----------|---------|
| W01  | 4        | 84         | 142      | 1       |
| W02  | 3        | 81         | 138      | 2       |
| W03  | 4        | 92         | 161      | 1       |
| W04  | 4        | 88         | 154      | 0       |
| W05  | 3        | 79         | 137      | 1       |
| W06  | 4        | 91         | 162      | 0       |
| W07  | 4        | 85         | 148      | 2       |
| W08  | 3        | 76         | 132      | 1       |
| W09  | 4        | 83         | 149      | 1       |
| W10  | 4        | 90         | 158      | 0       |
| W11  | 3        | 78         | 141      | 0       |
| W12  | 4        | 86         | 152      | 1       |
| W13  | 3        | 80         | 144      | 0       |

### 9.2 Customer-success motion

| Week | New ARR | Expansion | Churn | Net new |
|------|---------|-----------|-------|---------|
| W01  | 0.91    | 0.42      | 0.18  | 1.15    |
| W02  | 1.02    | 0.51      | 0.21  | 1.32    |
| W03  | 0.84    | 0.46      | 0.17  | 1.13    |
| W04  | 0.92    | 0.49      | 0.19  | 1.22    |
| W05  | 1.18    | 0.52      | 0.22  | 1.48    |
| W06  | 1.05    | 0.47      | 0.20  | 1.32    |
| W07  | 0.96    | 0.44      | 0.18  | 1.22    |
| W08  | 1.14    | 0.51      | 0.21  | 1.44    |
| W09  | 1.07    | 0.49      | 0.19  | 1.37    |
| W10  | 1.21    | 0.53      | 0.22  | 1.52    |
| W11  | 0.98    | 0.48      | 0.18  | 1.28    |
| W12  | 1.16    | 0.51      | 0.20  | 1.47    |
| W13  | 1.08    | 0.46      | 0.19  | 1.35    |

### 9.3 Capital-efficiency metrics

> Capital efficiency continues to improve. The "Bessemer Efficiency Score"
> (net new ARR divided by net cash burn) crossed above 1.0 in Q1 for the
> first time, indicating the business is now generating more new ARR
> than it consumes in cash to do so. This is the gating metric for the
> board's IPO-readiness criteria.

| Quarter | Magic Number | Burn Multiple | Bessemer Score |
|---------|--------------|---------------|----------------|
| Q1 2025 | 0.84         | 1.42          | 0.71           |
| Q2 2025 | 0.92         | 1.31          | 0.79           |
| Q3 2025 | 0.97         | 1.18          | 0.85           |
| Q4 2025 | 1.06         | 1.04          | 0.96           |
| Q1 2026 | 1.18         | 0.91          | 1.10           |

### 9.4 Marketing funnel

The integrated marketing funnel for the quarter:

| Stage             | Volume   | Conv. to next | Conv. to closed-won |
|-------------------|----------|---------------|---------------------|
| Visits            | 1.42M    | 4.1%          | 0.18%               |
| Sign-ups          | 58,200   | 42%           | 4.4%                |
| Activations       | 24,400   | 31%           | 10.5%               |
| SQLs              | 7,560    | 28%           | 33.7%               |
| Opportunities     | 2,120    | 51%           | 120%*               |
| Closed-won        | 1,082    | -             | -                   |

*Opportunity-to-close conversion exceeds 100% in some weeks because the
opportunity vintage skews older than the close vintage; reported on a
trailing-twelve-week basis.

### 9.5 Compliance posture

- **SOC 2 Type II** — surveillance audit completed 2026-02-21; clean.
- **ISO 27001:2022** — certification renewed 2026-03-04; no findings.
- **GDPR DPIA** — refreshed for the EU residency commit.
- **HIPAA** — readiness assessment complete; targeting attestation H2.

---

*Prepared by the Office of the CFO, in collaboration with the Engineering,
Product, and People organisations.*

[^methodology]: See section 8.3 for the methodology footnotes.
[^scope]: This document covers the parent entity and all wholly-owned subsidiaries.

---

## 10. Per-Region Operational Detail

The four operational regions are tracked individually below. Each region
is owned by a regional GM with P&L responsibility; the consolidated
numbers in section 1 roll up the per-region detail in this section.

### 10.1 Region: AMER

> AMER closed Q1 with 67% of new-logo ARR and 71% of total revenue.
> Mid-market grew the fastest at +29% YoY; SMB grew +12% YoY but
> remains the most price-sensitive segment.

| Segment   | NDR  | GDR | Logos | ARR (M) | New (M) |
|-----------|------|-----|-------|---------|---------|
| Enterprise| 126% | 97% | 84    | 51.8    | 4.2     |
| Mid-mkt   | 119% | 96% | 412   | 28.7    | 3.1     |
| SMB       | 104% | 89% | 2,810 | 15.4    | 1.6     |

#### Top accounts (AMER)

1. **Account Alpha** — 6.8M ARR; expansion +1.2M in Q1.
2. **Account Bravo** — 5.4M ARR; renewed flat.
3. **Account Charlie** — 4.9M ARR; expansion +0.4M in Q1.
4. **Account Delta** — 4.1M ARR; renewed flat.
5. **Account Echo** — 3.8M ARR; expansion +0.6M in Q1.

#### Pipeline (AMER, end of Q1)

| Stage           | Count | Weighted (M) |
|-----------------|-------|--------------|
| Discovery       | 142   | 7.1          |
| Validation      | 88    | 9.4          |
| Proposal        | 51    | 11.8         |
| Negotiation     | 28    | 8.6          |
| Closed-won (Q2) | 12    | 4.2          |

### 10.2 Region: EMEA

> EMEA grew +24% YoY, ahead of plan. The Northern Europe sub-region
> drove most of the upside, with the new Stockholm and Amsterdam
> account-executive hires already producing closed-won revenue.

| Segment   | NDR  | GDR | Logos | ARR (M) | New (M) |
|-----------|------|-----|-------|---------|---------|
| Enterprise| 121% | 96% | 38    | 16.4    | 1.8     |
| Mid-mkt   | 113% | 94% | 142   | 8.7     | 1.2     |
| SMB       | 100% | 86% | 982   | 4.6     | 0.6     |

#### Top accounts (EMEA)

1. **Account Foxtrot** — 3.2M ARR; expansion +0.4M in Q1.
2. **Account Golf** — 2.8M ARR; renewed +0.1M.
3. **Account Hotel** — 2.4M ARR; expansion +0.3M in Q1.
4. **Account India** — 2.1M ARR; renewed flat.
5. **Account Juliet** — 1.9M ARR; expansion +0.2M in Q1.

#### Pipeline (EMEA, end of Q1)

| Stage           | Count | Weighted (M) |
|-----------------|-------|--------------|
| Discovery       | 71    | 3.4          |
| Validation      | 44    | 4.8          |
| Proposal        | 26    | 6.2          |
| Negotiation     | 14    | 4.4          |
| Closed-won (Q2) | 6     | 2.1          |

### 10.3 Region: APAC

> APAC remains the smallest region but grew the fastest (+38% YoY).
> Japan is the strongest single country; Australia and Singapore continue
> to over-index in mid-market.

| Segment   | NDR  | GDR | Logos | ARR (M) | New (M) |
|-----------|------|-----|-------|---------|---------|
| Enterprise| 132% | 98% | 14    | 7.4     | 0.9     |
| Mid-mkt   | 124% | 97% | 58    | 3.1     | 0.5     |
| SMB       | 108% | 91% | 318   | 1.9     | 0.3     |

#### Top accounts (APAC)

1. **Account Kilo** — 2.0M ARR; expansion +0.3M in Q1.
2. **Account Lima** — 1.5M ARR; renewed +0.1M.
3. **Account Mike** — 1.2M ARR; expansion +0.2M in Q1.
4. **Account November** — 1.1M ARR; renewed flat.
5. **Account Oscar** — 0.9M ARR; expansion +0.1M in Q1.

#### Pipeline (APAC, end of Q1)

| Stage           | Count | Weighted (M) |
|-----------------|-------|--------------|
| Discovery       | 38    | 1.6          |
| Validation      | 22    | 2.1          |
| Proposal        | 11    | 2.4          |
| Negotiation     | 7     | 1.8          |
| Closed-won (Q2) | 3     | 0.7          |

### 10.4 Region: LATAM

> LATAM remains a long-term investment region. The Mexico City team is
> producing strong land-and-expand motion; Brazil opened later than
> planned but is now staffed.

| Segment   | NDR  | GDR | Logos | ARR (M) | New (M) |
|-----------|------|-----|-------|---------|---------|
| Enterprise| 117% | 95% | 6     | 2.8     | 0.4     |
| Mid-mkt   | 109% | 92% | 19    | 0.7     | 0.1     |
| SMB       | 96%  | 84% | 100   | 0.8     | 0.1     |

#### Top accounts (LATAM)

1. **Account Papa** — 0.9M ARR; expansion +0.1M in Q1.
2. **Account Quebec** — 0.7M ARR; renewed flat.
3. **Account Romeo** — 0.5M ARR; expansion +0.05M in Q1.

#### Pipeline (LATAM, end of Q1)

| Stage           | Count | Weighted (M) |
|-----------------|-------|--------------|
| Discovery       | 18    | 0.6          |
| Validation      | 9     | 0.7          |
| Proposal        | 4     | 0.8          |
| Negotiation     | 2     | 0.5          |
| Closed-won (Q2) | 1     | 0.2          |

---

## 11. Engineering Team Detail

Each engineering team submits a quarterly write-up covering velocity,
on-call posture, and notable shipped work. Excerpts below.

### 11.1 Team: Platform

> The Platform team focused on the v2 ingestion pipeline cutover and
> the new authorisation layer. Both projects shipped on time, and the
> ingestion cutover unlocked the v1 deprecation called out in section
> 2.3. The team also picked up two additional rotations on the auth
> on-call calendar to absorb load from the Application team.

Key shipped work:

- **v2 ingestion pipeline cutover** — 100% of new traffic now on v2.
- **RLS-aware authorisation layer** — replacing the legacy permissions
  cache.
- **Multi-region rate-limit cluster** — closes the auth single-region
  exposure called out in the Q4 2025 risk register.

```rust
// Representative API surface, simplified for the report.
pub async fn ingest(
    payload: Payload,
    tenant: &Tenant,
) -> Result<Receipt, IngestError> {
    let normalised = normalise(payload, tenant)?;
    let receipt = pipeline::v2::accept(normalised, tenant).await?;
    Ok(receipt)
}
```

### 11.2 Team: Application

> Application shipped Universal Inbox GA, Reporting v2 GA, and made
> meaningful progress on Workflow Studio v3. The slip on the latter
> was driven by a deliberate scope addition (canvas-renderer rewrite)
> rather than execution risk.

Key shipped work:

- **Universal Inbox GA** — beta to GA in 8 weeks; in-app onboarding
  shipped alongside.
- **Reporting v2 GA** — 63% of eligible customers migrated within 8
  weeks.
- **Mobile push for SOC 2 alerts** — partner team for the security buyer
  persona.

### 11.3 Team: Data and ML

> Data and ML completed the MongoDB to Postgres migration on the
> audit-log service and picked up the customer-churn-prediction model
> work that the Customer Success org had been waiting on. The model
> now powers the account-health scoring referenced in section 4.1.

Key shipped work:

- **Audit-log MongoDB to Postgres migration** — complete.
- **Account-health model v2** — informing CSM renewal-touch coverage.
- **Reporting v2 backend** — feature-complete; analytics warehouse
  optimisation continues.

### 11.4 Team: Infrastructure

> Infrastructure absorbed the v1 ingestion deprecation work in
> partnership with Platform, retired the dual-write to the analytics
> warehouse, and stood up the second cloud region for the
> data-residency commit.

Key shipped work:

- **EU region (FRA)** — stood up; pilot tenants migrated.
- **Dual-write removal** — CDC pipeline is now the sole writer.
- **Cost-optimisation initiative** — net cloud spend down 14% QoQ
  excluding new-region build-out.

### 11.5 Team: Security

> Security closed the SOC 2 surveillance audit clean, refreshed the
> ISO 27001:2022 certification, and stood up the HIPAA readiness work
> for H2 attestation. The team also led the dual-control-on-prod-config
> incident response described in section 2.2.

Key shipped work:

- **Dual-control on prod config pushes** — incident-response output.
- **HIPAA readiness assessment** — complete; H2 attestation on plan.
- **SOC 2 surveillance audit** — clean.

---

## 12. Customer Voice

A sample of qualitative customer feedback collected via the
quarterly business-review motion. Quotes are anonymised but
attributable on request.

> *"The new Universal Inbox has cut our team's daily mail-triage time
> from 40 minutes to under 10. Our security analysts get to the
> alerts that actually matter, faster. We're seeing tickets-to-incident
> ratios drop noticeably."*
> — Director of Security Engineering, Enterprise customer

> *"Reporting v2 is genuinely a step change. The custom-metric API
> means we no longer have to ship our finance data into a separate
> dashboard tool. One less integration, one less vendor."*
> — Head of Finance Operations, Mid-market customer

> *"Workflow Studio v3's branching primitives are the thing we've
> been waiting for since 2024. We understand the slip; the quality
> bar held. We'd rather have it right than fast."*
> — VP Engineering, Enterprise customer

> *"Support has gotten faster every quarter. Our last critical was
> resolved in under an hour, and the post-incident communication
> was clear and direct. We notice."*
> — CTO, Mid-market customer

---

## 13. Action Items for Q2

The leadership team owns the following actions for Q2. Owners and
deadlines are tracked in the Q2 ops review document.

- [ ] Workflow Studio v3 ships by end of week 4.
- [ ] Cross-workspace search ships by end of Q2.
- [ ] Second payment-processor integration GA by end of Q2.
- [ ] Read-only audit-log API GA by end of Q2.
- [ ] SAML group-attribute mapping ships in Q2.
- [x] Q1 audit-log MongoDB to Postgres migration — complete.
- [x] v1 ingestion pipeline deprecated — complete.
- [x] Multi-region rate-limit cluster — complete.

---

*End of report.*

---

## 14. Quarterly Cross-Functional Project Index

The cross-functional project index below summarises every project that
spanned more than one organisation in the quarter. Project status is
captured at end-of-quarter; the live tracker carries the running view.

### 14.1 Project: Universal Inbox GA

> **Status:** Shipped on time, 2026-03-12. Adoption running ~2x the
> comparable reference launch from 2025. Seven of the top-ten customer
> deployments completed onboarding within the first three weeks.

| Workstream            | Owner        | Status     |
|-----------------------|--------------|------------|
| Backend infra cutover | Platform     | Complete   |
| Inbox UI              | Application  | Complete   |
| In-app onboarding     | Application  | Complete   |
| Marketing site update | Marketing    | Complete   |
| Support enablement    | Support      | Complete   |
| Pricing/packaging     | Finance      | Complete   |

### 14.2 Project: Reporting v2 GA

> **Status:** Shipped on time, 2026-02-04. Migration uptake ahead of
> plan. Long-tail of legacy reports still served from v1 backend; v1
> read-path deprecation now scheduled for Q3.

| Workstream              | Owner        | Status     |
|-------------------------|--------------|------------|
| v2 backend              | Data and ML  | Complete   |
| Custom-metric API       | Application  | Complete   |
| Migration tooling       | Application  | Complete   |
| Customer migration ops  | CSM          | In progress|
| v1 read-path deprecation| Application  | Q3         |

### 14.3 Project: Workflow Studio v3

> **Status:** Slipped to Q2 week 4. Slip driven by deliberate scope
> addition (canvas renderer rewrite). Decision and rationale ratified
> by the product council on 2026-02-18.

| Workstream              | Owner        | Status     |
|-------------------------|--------------|------------|
| Canvas renderer rewrite | Application  | In progress|
| Branching primitives    | Application  | Complete   |
| Migration tooling       | Application  | In progress|
| Beta cohort onboarding  | CSM          | Q2         |

### 14.4 Project: EU data residency

> **Status:** Region stood up, pilot tenants migrated. Production
> traffic cutover gated on the legal sign-off for the new sub-processor
> agreements; targeted for end of Q2.

| Workstream                | Owner          | Status     |
|---------------------------|----------------|------------|
| FRA region build-out      | Infrastructure | Complete   |
| Multi-region routing      | Platform       | Complete   |
| Pilot tenant migration    | CSM            | Complete   |
| Sub-processor agreements  | Legal          | In progress|
| GA cutover comms          | Marketing      | Q2         |

### 14.5 Project: HIPAA readiness

> **Status:** Readiness assessment complete; gap remediation under way.
> Targeting H2 attestation. The work overlaps with the SOC 2 evidence
> base; ~70% of controls are already in scope and in place.

| Workstream                | Owner       | Status     |
|---------------------------|-------------|------------|
| Readiness assessment      | Security    | Complete   |
| Gap remediation           | Security    | In progress|
| Customer-facing BAA       | Legal       | In progress|
| Attestation engagement    | Security    | H2         |

### 14.6 Project: Second payment-processor integration

> **Status:** Engineering complete; finance-side UAT under way.
> Targeted GA mid-Q2. The dual-PSP architecture follows the pattern
> documented in the 2025 incident-response review, and addresses the
> top item in the risk register.

| Workstream              | Owner       | Status     |
|-------------------------|-------------|------------|
| Engineering integration | Platform    | Complete   |
| Finance UAT             | Finance     | In progress|
| Reconciliation reporting| Finance     | In progress|
| Customer-facing comms   | Marketing   | Q2         |

---

## 15. Operational Risk Detail

The risk register in section 5 is the leadership-level summary. The
operational-risk detail below is the program office's working list,
including items that have been triaged below the leadership threshold
but remain on the radar.

### 15.1 Operational risks (working list)

| ID  | Risk                                          | Likelihood | Impact | Owner       | Status     |
|-----|-----------------------------------------------|------------|--------|-------------|------------|
| R01 | Single-PSP concentration                      | Medium     | High   | Finance     | Mitigating |
| R02 | EU cross-border transfers regulatory ruling   | Medium     | High   | Legal       | Watching   |
| R03 | Senior-engineer attrition (specific team)     | Medium     | Medium | Engineering | Mitigating |
| R04 | Data-warehouse capacity for new analytics    | Low        | Medium | Data and ML | Mitigating |
| R05 | Vendor SLA on observability platform         | Low        | Medium | Infra       | Watching   |
| R06 | New EU sub-processor agreements timeline     | Medium     | Medium | Legal       | Mitigating |
| R07 | Sales-comp plan consistency in new region    | Low        | Low    | GTM         | Watching   |
| R08 | Dependency on one CRM vendor                 | Low        | Medium | RevOps      | Watching   |
| R09 | Dependency on one ID-provider for SSO        | Low        | Medium | Security    | Watching   |
| R10 | Capacity for SOC 2 evidence collection       | Low        | Low    | Security    | Mitigating |
| R11 | Data-residency for non-EU regulated tenants  | Low        | Medium | Infra       | Backlog    |
| R12 | New-region tax-and-employment posture        | Low        | Medium | Finance     | Mitigating |

### 15.2 Risk-treatment notes

- **R01 (PSP concentration).** Engineering complete on the second-PSP
  integration. UAT in finance-side reconciliation reporting. GA mid-Q2.
- **R02 (EU transfers).** Watching the pending court decision. The EU
  residency build-out (project 14.4) is the long-term hedge.
- **R03 (Engineering attrition).** Backfill recruiting in flight.
  Knowledge-share rotation set up. Succession plans documented.
- **R04 (Warehouse capacity).** Tier-up to next compute tier scheduled
  for Q2. Cost impact captured in the Q2 plan.
- **R05 (Observability vendor).** Reviewing two alternatives in case
  the current vendor's SLA terms do not improve at renewal.
- **R06 (EU sub-processors).** New agreements in legal review. Targeted
  signature end of Q2 to support project 14.4 GA.
- **R07 (Comp consistency).** Q2 comp-plan refresh will normalise.
- **R08 (CRM vendor).** Lower priority. Migration cost would be high.
- **R09 (ID provider).** Architectural review scheduled for H2.
- **R10 (Evidence capacity).** Tooling automation reduced manual
  evidence collection by ~40% in Q1. Continuing.
- **R11 (Non-EU regulated tenants).** Off-roadmap; will revisit when
  customer demand crosses threshold.
- **R12 (Tax and employment).** Engaged outside counsel; on track.

---

## 16. People Operations Detail

### 16.1 Hiring throughput

| Function    | Open Q1-start | Filled | Closed | Open Q1-end |
|-------------|---------------|--------|--------|-------------|
| Engineering | 14            | 16     | -8*    | 8           |
| Product     | 3             | 2      | -1     | 0           |
| Design      | 1             | 1      | 0      | 0           |
| GTM         | 7             | 4      | 0      | 3           |
| G and A     | 2             | 1      | 0      | 1           |

*Eight engineering reqs were closed without a hire because the work
was de-scoped during the Q1 portfolio review; the remaining open reqs
roll forward into Q2.

### 16.2 Compensation review

The Q1 comp review applied the standard methodology: market-data refresh
from the comp-survey vendor, internal-equity check, manager calibration,
and finance approval. Notable outputs:

- 92% of in-band employees received a market-aligned increase.
- 8% of employees were identified as below-band and received a
  band-correction increase outside the standard window.
- No off-cycle equity grants outside the planned refresh program.

### 16.3 Performance

The Q1 performance check-in cycle ran on the new lighter-touch model
(quarterly conversation, no formal calibration). Manager NPS on the
new model: +42. Employee NPS on the new model: +38. Both materially
above the prior-cycle scores.

### 16.4 Diversity, equity, and inclusion

| Metric                                | Q1 2026 | Q1 2025 |
|---------------------------------------|---------|---------|
| Women in engineering                  | 28%     | 24%     |
| Women in leadership (M+ band)         | 41%     | 37%     |
| URM in US workforce                   | 22%     | 19%     |
| Pay-gap audit (gender, controlled)    | <0.5%   | <0.5%   |
| Pay-gap audit (URM, controlled)       | <0.5%   | <0.5%   |

---

## 17. Appendix: Methodology and Definitions

### 17.1 Reporting cadence

- **Daily**: a single-page operational dashboard distributed to the
  leadership team.
- **Weekly**: a 4-page weekly business review distributed to all
  managers.
- **Monthly**: a closed-financials packet distributed to the board
  observer list.
- **Quarterly**: this report (consolidated operational view) plus the
  audited financial statements (reconciled in the 10-business-day
  window after quarter-end).

### 17.2 Source-of-truth systems

- **CRM** is the source of truth for opportunity stage, ARR, and
  closed-won bookings.
- **ERP** is the source of truth for revenue, cost, and cash flow.
- **HRIS** is the source of truth for headcount, attrition, and comp.
- **Reporting warehouse** is the source of truth for the operational
  metrics in sections 2-4 and 9-12.

### 17.3 Restatement policy

We restate prior-period numbers when:

1. A material reclassification changes the comparability of a metric.
2. An audited adjustment is finalised after the original publication.
3. A definitional change is approved by the program office.

All restatements are flagged in the next-quarter report's appendix and
reflected in the rolling trend file.

### 17.4 Glossary additions for Q1

- **Bessemer Efficiency Score**: net new ARR divided by net cash burn,
  per the Bessemer Venture Partners definition.
- **Magic Number**: net new ARR for the quarter divided by sales and
  marketing spend in the prior quarter, annualised.
- **Burn Multiple**: net cash burn divided by net new ARR for the
  same period.

### 17.5 Approvals

This report was reviewed by the leadership team on 2026-04-09, ratified
by the CFO on 2026-04-10, and circulated to the broader management
team on 2026-04-11.

---

*Final.*

---

## 18. Per-Week Operational Detail

The per-week detail below archives the weekly business-review numbers
for the quarter. Future quarters can compare against these directly
without re-querying the warehouse.

### 18.1 Week 1 (2025-12-30 to 2026-01-05)

> Holiday-shortened week. Pipeline-build motion paused; on-call
> coverage held the line. No production incidents.

| Metric             | Actual | Plan  | Delta |
|--------------------|--------|-------|-------|
| New ARR            | 0.91   | 0.80  | +13%  |
| Pipeline created   | 4.2    | 4.0   | +5%   |
| Tickets opened     | 1,108  | 1,200 | -8%   |
| Tickets resolved   | 1,142  | 1,200 | -5%   |
| P0 incidents       | 0      | 0     | -     |
| P1 incidents       | 0      | <1    | -     |

### 18.2 Week 2 (2026-01-06 to 2026-01-12)

> First full operating week. Sales QBRs ran through the week. One P1
> incident (transient ingestion lag, 18 minutes), resolved within SLO.

| Metric             | Actual | Plan  | Delta |
|--------------------|--------|-------|-------|
| New ARR            | 1.02   | 0.95  | +7%   |
| Pipeline created   | 4.8    | 4.7   | +2%   |
| Tickets opened     | 1,420  | 1,400 | +1%   |
| Tickets resolved   | 1,398  | 1,400 | -0%   |
| P0 incidents       | 0      | 0     | -     |
| P1 incidents       | 1      | <1    | -     |

### 18.3 Week 3 (2026-01-13 to 2026-01-19)

> Net-new ARR slightly soft on a quieter renewals week. No incidents.

| Metric             | Actual | Plan  | Delta |
|--------------------|--------|-------|-------|
| New ARR            | 0.84   | 0.95  | -12%  |
| Pipeline created   | 4.4    | 4.7   | -6%   |
| Tickets opened     | 1,372  | 1,400 | -2%   |
| Tickets resolved   | 1,401  | 1,400 | +0%   |
| P0 incidents       | 0      | 0     | -     |
| P1 incidents       | 0      | <1    | -     |

### 18.4 Week 4 (2026-01-20 to 2026-01-26)

> Recovered from the prior week's softness. Reporting v2 dogfooding
> sprint produced eight customer-issue tickets (all addressed).

| Metric             | Actual | Plan  | Delta |
|--------------------|--------|-------|-------|
| New ARR            | 0.92   | 0.95  | -3%   |
| Pipeline created   | 5.1    | 4.7   | +9%   |
| Tickets opened     | 1,508  | 1,400 | +8%   |
| Tickets resolved   | 1,471  | 1,400 | +5%   |
| P0 incidents       | 0      | 0     | -     |
| P1 incidents       | 0      | <1    | -     |

### 18.5 Week 5 (2026-01-27 to 2026-02-02)

> Strongest week of the quarter for new ARR. Reporting v2 GA-readiness
> review held mid-week; green-lit for the following Monday.

| Metric             | Actual | Plan  | Delta |
|--------------------|--------|-------|-------|
| New ARR            | 1.18   | 1.00  | +18%  |
| Pipeline created   | 5.4    | 5.0   | +8%   |
| Tickets opened     | 1,620  | 1,500 | +8%   |
| Tickets resolved   | 1,584  | 1,500 | +6%   |
| P0 incidents       | 0      | 0     | -     |
| P1 incidents       | 0      | <1    | -     |

### 18.6 Week 6 (2026-02-03 to 2026-02-09)

> Reporting v2 GA shipped Tuesday. Migration uptake started ahead of
> plan. One P1 incident (rate-limit cluster cascading restart),
> resolved within 4 minutes; post-incident review filed.

| Metric             | Actual | Plan  | Delta |
|--------------------|--------|-------|-------|
| New ARR            | 1.05   | 1.00  | +5%   |
| Pipeline created   | 5.0    | 5.0   | -1%   |
| Tickets opened     | 1,742  | 1,500 | +16%  |
| Tickets resolved   | 1,683  | 1,500 | +12%  |
| P0 incidents       | 0      | 0     | -     |
| P1 incidents       | 1      | <1    | -     |

### 18.7 Week 7 (2026-02-10 to 2026-02-16)

> Mid-quarter forecast call: forecast on track. Universal Inbox beta
> opened to the second cohort (200 customers).

| Metric             | Actual | Plan  | Delta |
|--------------------|--------|-------|-------|
| New ARR            | 0.96   | 1.00  | -4%   |
| Pipeline created   | 5.2    | 5.0   | +5%   |
| Tickets opened     | 1,612  | 1,500 | +7%   |
| Tickets resolved   | 1,648  | 1,500 | +10%  |
| P0 incidents       | 0      | 0     | -     |
| P1 incidents       | 0      | <1    | -     |

### 18.8 Week 8 (2026-02-17 to 2026-02-23)

> Workflow Studio v3 scope-decision week. Product council ratified the
> canvas-renderer rewrite addition; slip to Q2 acknowledged.

| Metric             | Actual | Plan  | Delta |
|--------------------|--------|-------|-------|
| New ARR            | 1.14   | 1.00  | +14%  |
| Pipeline created   | 5.3    | 5.0   | +6%   |
| Tickets opened     | 1,540  | 1,500 | +3%   |
| Tickets resolved   | 1,576  | 1,500 | +5%   |
| P0 incidents       | 0      | 0     | -     |
| P1 incidents       | 0      | <1    | -     |

### 18.9 Week 9 (2026-02-24 to 2026-03-02)

> Strong renewals week. NDR cohort attainment tracking ahead of plan.

| Metric             | Actual | Plan  | Delta |
|--------------------|--------|-------|-------|
| New ARR            | 1.07   | 1.05  | +2%   |
| Pipeline created   | 5.0    | 5.2   | -3%   |
| Tickets opened     | 1,448  | 1,500 | -3%   |
| Tickets resolved   | 1,512  | 1,500 | +1%   |
| P0 incidents       | 0      | 0     | -     |
| P1 incidents       | 0      | <1    | -     |

### 18.10 Week 10 (2026-03-03 to 2026-03-09)

> Universal Inbox GA-readiness review held Friday; green-lit for the
> following week.

| Metric             | Actual | Plan  | Delta |
|--------------------|--------|-------|-------|
| New ARR            | 1.21   | 1.05  | +15%  |
| Pipeline created   | 5.6    | 5.2   | +8%   |
| Tickets opened     | 1,602  | 1,500 | +7%   |
| Tickets resolved   | 1,584  | 1,500 | +6%   |
| P0 incidents       | 0      | 0     | -     |
| P1 incidents       | 0      | <1    | -     |

### 18.11 Week 11 (2026-03-10 to 2026-03-16)

> Universal Inbox GA shipped Thursday. Onboarding-flow telemetry
> indicated higher-than-expected completion rates from minute one.

| Metric             | Actual | Plan  | Delta |
|--------------------|--------|-------|-------|
| New ARR            | 0.98   | 1.05  | -7%   |
| Pipeline created   | 5.1    | 5.2   | -2%   |
| Tickets opened     | 1,704  | 1,500 | +14%  |
| Tickets resolved   | 1,672  | 1,500 | +11%  |
| P0 incidents       | 0      | 0     | -     |
| P1 incidents       | 0      | <1    | -     |

### 18.12 Week 12 (2026-03-17 to 2026-03-23)

> First full week of Universal Inbox GA-traffic. Native Slack threading
> shipped to the first launch cohort.

| Metric             | Actual | Plan  | Delta |
|--------------------|--------|-------|-------|
| New ARR            | 1.16   | 1.10  | +6%   |
| Pipeline created   | 5.4    | 5.2   | +4%   |
| Tickets opened     | 1,651  | 1,500 | +10%  |
| Tickets resolved   | 1,684  | 1,500 | +12%  |
| P0 incidents       | 0      | 0     | -     |
| P1 incidents       | 0      | <1    | -     |

### 18.13 Week 13 (2026-03-24 to 2026-03-30)

> Quarter-close week. Mobile push for SOC 2 alerts shipped Friday.

| Metric             | Actual | Plan  | Delta |
|--------------------|--------|-------|-------|
| New ARR            | 1.08   | 1.10  | -2%   |
| Pipeline created   | 5.0    | 5.2   | -3%   |
| Tickets opened     | 1,433  | 1,500 | -4%   |
| Tickets resolved   | 1,461  | 1,500 | -3%   |
| P0 incidents       | 0      | 0     | -     |
| P1 incidents       | 0      | <1    | -     |

---

*End of per-week detail.*

---

## 19. Closing Remarks

This is the last quarterly report on the v1 reporting template. Starting
in Q2, we'll publish the operational view in the new format ratified by
the program office in March. The headline-level metrics will stay
consistent so trend-over-trend comparisons remain straightforward; the
detail sections will reorganise around the four operating regions
rather than around the four functional organisations.

The leadership team thanks the broader management group for the
hands-on contribution to this report's data and qualitative content.
The next quarterly view will land 10 business days after Q2 close, with
the customary executive read-out on the second Friday after publication.

### 19.1 Acknowledgements

- Engineering leadership team — for the section 11 write-ups.
- CSM leadership — for the customer-voice section 12 contributions.
- Program office — for the cross-functional project index and the
  operational-risk detail.
- Finance — for the rapid-turnaround financial highlights and the
  capital-efficiency analysis.
- People operations — for the headcount, attrition, and engagement
  detail.
- Security — for the compliance-posture section.

### 19.2 Distribution

This report is distributed to:

- Board of directors and observers (full).
- Leadership team (full).
- Broader management team (full, redacted version on request).
- All-hands snapshot (financial highlights and outlook only).
- Investor update (financial highlights and outlook only).

### 19.3 Feedback

Feedback on this report is welcome and is collated by the program office
into the next-quarter template-refresh exercise. Please send comments
or corrections by end of the second week of Q2.

---

*Final. Q1 2026 Operations Report.*
