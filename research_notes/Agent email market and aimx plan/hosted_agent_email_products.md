# Hosted "Email for AI Agents" Products and Email APIs with MCP (as of 16 Oct 2026)

Method note: research done 16 Oct 2026. Direct page fetches were blocked for most vendor domains in this environment (only github.com was fetchable), so many facts come from web-search summaries of vendor pages plus third-party comparison posts. **Many comparison posts are written by competing agent-email vendors** (AgentMail, Dead Simple Email, LobsterMail, Bavimail, Lumbox, Mailtrap, OpenMail, Shipmail). Treat their numbers as vendor-reported and check them before relying on them. GitHub star counts were read directly from github.com on 16 Oct 2026.

## Q1. Which products are purpose-built for AI agent inboxes, and how do they compare? (dense competitive table)

### Takeaway
A crowded category formed in 2025-2026. **AgentMail** (YC S25, $6M seed, Mar 2026) leads the purpose-built "inbox API for agents" segment. Since April 2026 it has been squeezed from three directions:
- **Platforms:** Cloudflare Email Service plus the Agents SDK, and the open-source Agentic Inbox.
- **Transactional ESPs bolting on agent inboxes:** Resend Inboxes (private beta), Mailtrap Inbound (GA June 2026), Postmark inbound.
- **Big identity platforms:** Nylas Agent Accounts (GA June 2026), Microsoft Entra agent users, and Google's Gemini agents with @agents addresses (private preview, Oct 2026).

A long tail of small 2026 startups competes on price per inbox. None of the hosted products lets you self-host the data. The closest is Cloudflare's Apache-2.0 Agentic Inbox, which runs in *your* Cloudflare account and has about 8.3k GitHub stars.

### Table A — Capabilities

| Product | Type | Inbound | Outbound | Custom domain | MCP (official? transport; tools) | Real-time | Threading / attachments | Self-host data? |
|---|---|---|---|---|---|---|---|---|
| **AgentMail** (agentmail.to) | Purpose-built agent inbox API | Yes ([docs](https://docs.agentmail.to/knowledge-base/inbox-capabilities)) | Yes; also IMAP/SMTP access to each inbox ([docs](https://docs.agentmail.to/knowledge-base/inbox-capabilities)) | Yes on paid plans (TXT/CNAME/MX verify; wildcard-MX subdomains) ([changelog](https://docs.agentmail.to/changelog?filter=websockets)); not on Free ([usecarly](https://www.usecarly.com/blog/agentmail-pricing/)) | Official. Remote Streamable HTTP `https://mcp.agentmail.to/mcp` (OAuth or API key), plus stdio bridges `npx agentmail-mcp` / `uvx agentmail-mcp` that pull tools from the hosted server ([GitHub](https://github.com/agentmail-to/agentmail-mcp)). Docs say 43 tools ([docs](https://docs.agentmail.to/integrations/mcp)); core loop is `create_inbox`, `send_message`, `list_threads`, `reply_to_message` ([blog](https://www.agentmail.to/blog/how-to-use-mcp-with-an-email-api)) | Webhooks + WebSockets (no public URL needed) ([docs](https://docs.agentmail.to/websockets)) | Auto-threading on Message-ID / In-Reply-To / References; labels; base64 attachments; allow/blocklists ([docs](https://docs.agentmail.to/knowledge-base/inbox-capabilities)) | No |
| **Cloudflare Email Service + Email Routing + Agents SDK** | Platform primitives | Email Routing: free, unlimited, but the domain must use Cloudflare DNS; **no storage or mailboxes** ([lushbinary](https://lushbinary.com/blog/cloudflare-email-service-vs-aws-ses-agent-email-guide/), [usecarly](https://www.usecarly.com/blog/cloudflare-email-service/)) | Email Sending (public beta 16 Apr 2026) ([CF blog](https://blog.cloudflare.com/email-for-agents/)) | Required (CF DNS) | Agents can call Email endpoints through the general Cloudflare MCP server ([usecarly](https://www.usecarly.com/blog/cloudflare-email-service/)). The Agents SDK has `onEmail`, `routeAgentEmail`, address-based resolvers, and `replyToEmail` with an HMAC-signed secure-reply resolver; each agent is a Durable Object ([CF docs](https://developers.cloudflare.com/agents/api-reference/email), [example](https://developers.cloudflare.com/agents/examples/email-agent/)) | Worker `email` handler | `replyToEmail` sets In-Reply-To/References ([CF docs](https://developers.cloudflare.com/agents/api-reference/email)) | Code runs in your CF account; data stays in CF |
| **Cloudflare Agentic Inbox** (OSS reference app) | Self-deployable agent inbox | Via Email Routing; one Durable Object + SQLite per mailbox | Via Email Service | Required | Has an MCP endpoint at `/mcp` (takes `mailboxId`); built-in agent has 9 email tools; **sending always needs explicit confirmation** ([GitHub](https://github.com/cloudflare/agentic-inbox)) | — | Threading, folders, search; attachments in R2 ([GitHub](https://github.com/cloudflare/agentic-inbox)) | Yes, in your own CF account (Apache-2.0) |
| **Resend** | Transactional ESP, adding agent inboxes | Receiving is webhook-based ([docs](https://resend.com/docs/dashboard/receiving/introduction)). **Inboxes API (private beta)** adds inboxes, threads, labels, drafts ([usecarly](https://www.usecarly.com/blog/resend-pricing/), [resend-cli PR](https://github.com/resend/resend-cli/pull/401)) | Yes | Yes | Official `resend/resend-mcp`: stdio (`npx -y resend-mcp`), self-run HTTP, and hosted `https://mcp.resend.com/mcp` (OAuth/Bearer). 18 tool groups including Received Emails and Inboxes ([GitHub](https://github.com/resend/resend-mcp)). Inboxes tools are on the remote server only ([AgentMail blog](https://www.agentmail.to/blog/email-mcp-servers-for-ai-agents)) | Webhooks (with replay and secret rotation tools) ([GitHub](https://github.com/resend/resend-mcp)) | Inbox threads; received-attachment download ([GitHub](https://github.com/resend/resend-mcp)) | No. All data stored in the US; cannot commit to EU-only storage ([soleur #9459](https://github.com/jikig-ai/soleur/issues/9459)) |
| **Postmark** (ActiveCampaign) | Transactional ESP | Inbound JSON webhook on Pro/Platform plans only ([klymentiev](https://klymentiev.com/blog/postmark-pricing)) | Yes | Yes | Official `ActiveCampaign/postmark-mcp`: stdio only, **24 tools, all outbound or management** (send, templates, search outbound, bounces, suppressions, stats, webhooks). **No inbound read tools** ([GitHub](https://github.com/ActiveCampaign/postmark-mcp)) | Webhooks | N/A in MCP | No |
| **Mailgun** (Sinch; ownership not verified this session) | Transactional ESP | Routes (inbound rules) | Yes | Yes | Official `mailgun/mailgun-mcp-server`: **stdio only, no hosted version**. Categories include Routes (view/update), Messaging, Domains, Webhooks, Templates, Analytics. No delete ops ([GitHub](https://github.com/mailgun/mailgun-mcp-server)). Docs claim 50+ ops ([docs](https://documentation.mailgun.com/docs/mailgun/mcp)) | Webhooks | — | No |
| **Twilio SendGrid** | Transactional ESP | Inbound Parse: you set the MX and host the endpoint; it won't follow redirects ([Twilio docs](https://www.twilio.com/docs/sendgrid/for-developers/parsing-email/setting-up-the-inbound-parse-webhook)) | Yes | Yes | **No official MCP that executes actions.** Twilio's hosted MCP only indexes docs, read-only ([Scalekit](https://www.scalekit.com/blog/sendgrid-mcp-vs-api)). Composio runs a third-party SendGrid MCP that includes parse settings ([Composio](https://composio.dev/toolkits/sendgrid)) | Webhook POST | Parse payload | No |
| **Amazon SES + Mail Manager** | Cloud ESP | Mail Manager ingress → rules → actions ([AWS blog](https://aws.amazon.com/blogs/messaging-and-targeting/getting-started-with-amazon-ses-agent-skills-for-ai-assisted-email-development/)) | Yes | Yes | Sept 2026: "agent skills" for the general AWS MCP Server; a separate `aws-mail-manager` skill covers inbound ([AWS What's New](https://aws.amazon.com/about-aws/whats-new/2026/09/aws-messaging-ses-ai-skills-mcp-server/), [SES docs](https://docs.aws.amazon.com/ses/latest/dg/agent-setup-guide.html)). The earlier SES MCP was an unsupported sample ([Mailtrap](https://mailtrap.io/blog/best-email-mcp-servers/), ~Aug 2026) | SNS/Lambda (not verified this session) | DIY | Your AWS account |
| **Mailtrap Agent Inbox / Inbound** | ESP + agent inbox | Inbound GA 2 Jun 2026 at `@inbound-mailtrap.io` addresses; **custom receiving domains "coming soon"** ([changelog](https://feedback.mailtrap.io/changelog/inbound-is-generally-available)) | Yes | Sending yes; receiving not yet | Official MCP with **19 inbound tools** (create inbox, list/get/delete, reply, reply-all, forward, threads) ([Mailtrap](https://mailtrap.io/blog/best-inbound-email-api/), [Agent Inbox](https://mailtrap.io/agent-inbox/)). Total tool count conflicts across sources: 106 / 117 / 24 / 15 | Webhook (HMAC-signed) | Thread ID, attachment URLs; no spam verdict in payload ([Mailtrap](https://mailtrap.io/blog/best-inbound-email-api/)) | No |
| **Nylas Agent Accounts** | Hosted mailbox + calendar per agent | Yes | Yes | Yes (`name@yourdomain.com`) ([changelog](https://developer.nylas.com/docs/changelogs/2026-06-17-agent-accounts-ga/)) | Hosted MCP at `mcp.us.nylas.com`, plus a CLI ([Yahoo Finance/launch](https://finance.yahoo.com/technology/ai/articles/nylas-launches-agent-accounts-giving-153200500.html)) | Webhooks (same grant-based API) ([changelog](https://developer.nylas.com/docs/changelogs/2026-06-17-agent-accounts-ga/)) | Messages/threads via the Nylas API | No |
| **MailSlurp** | Test inbox API | Yes | Limited (500 external sends/mo on Pro) ([pricing](https://app.mailslurp.com/pricing/)) | Yes | Disputed. Composio hosts a MailSlurp MCP ([Composio](https://composio.dev/toolkits/mailslurp)); competitors say there is no native MCP ([MailSink](https://mailsink.dev/blog/mailsink-vs-mailslurp/)) | Webhooks | — | No |
| **inbound.new** | Inbound-first API "for agents and indie devs" | Programmable addresses → webhooks; spam filtering ([GitHub](https://github.com/inboundemail/inbound)) | Yes, plus a `reply` method | Yes (DNS management) | Not mentioned in repo ([GitHub](https://github.com/inboundemail/inbound)) | Signed webhooks | Attachments in S3 ([GitHub](https://github.com/inboundemail/inbound)) | Partly. MIT repo with a local Docker dev setup, but no production self-host guide |
| **Inkbox** (YC S26) | Agent identity: email + phone + iMessage | Yes | Yes | Yes | Plugins for Hermes, OpenClaw, Claude ([YC launch](https://www.ycombinator.com/launches/RIu-inkbox-give-your-ai-agents-email-phone-imessage-and-an-internet-address)) | Persistent HTTPS tunnel for webhooks | — | No |
| **OpenMail** (openmail.sh) | Agent inbox API, EU data residency on all plans | Yes | Yes | — | Not verified | Webhooks ([site](https://openmail.sh/)) | — | No (EU-hosted) ([OpenMail](https://openmail.sh/compare/agentmail-alternative)) |
| **Atomic Mail** | Agent inbox on open JMAP | Yes | Yes | — | Not verified | — | — | Agents self-register via proof-of-work; free in open alpha ([Atomic Mail](https://atomicmail.io/atomic-mail-vs-agentmail)) |
| **Robotomail, LobsterMail, Mails.ai, Dead Simple Email, Bavimail, Lumbox, Shipmail, AgentEmail, AgenticEmail** | Small 2026 agent-inbox startups | Yes (vendor-reported) | Yes | Mostly | Varies. One review says Robotomail has no documented MCP ([lumbox](https://lumbox.co/best-email-api-for-ai-agents/)) | — | — | No ([deadsimple](https://deadsimple.email/compare/pricing.html), [hunted.space AgentEmail](https://hunted.space/product/agentemail), [dev.to AgenticEmail](https://dev.to/agenticemail/give-your-ai-agent-a-real-inbox-407g)) |
| **Microsoft Entra "agent users"** | Enterprise identity | An M365 mailbox for an agent user (an Entra user paired 1:1 with an agent identity) ([Microsoft Learn](https://learn.microsoft.com/en-us/entra/identity/conditional-access/policy-agent-user), [Entrabot](https://microsoft.github.io/entrabot/platform-docs/entra-agent-users/)) | Yes | Tenant domain | Via Graph. One report says agent identities got a 401 on Exchange Graph calls, "appears not to be supported" ([MS Q&A](https://learn.microsoft.com/en-us/answers/questions/5951873/do-entra-agent-id-agent-identities-support-exchang)) | Graph | Exchange | No |
| **Google Gemini agents (Workspace)** | Enterprise identity | Each "coworker" agent gets its own Workspace account with an `@agents` address, calendar, Drive, and directory entry ([dragapp](https://www.dragapp.com/blog/gemini-agent/)) | Yes | Workspace domain | — | — | Gmail | No |
| **Lindy** | Agent platform | "Lindy Mail" gives an agent its own address to send and receive ([Lindy docs](https://docs.lindy.ai/skills/by-lindy/lindy-mail)) | Yes | — | — | Trigger on reply in thread | Same-thread replies | No |
| **Unipile / Composio / Zapier MCP / Pipedream** | Connectors to *existing* human mailboxes (Gmail/Outlook/IMAP) | Through user's mailbox | Yes | N/A | Unipile launched an MCP in 2026 ([Unipile](https://www.unipile.com/it/mcp/)). Zapier MCP bills 2 tasks per tool call ([latenode](https://latenode.com/blog/zapier-mcp-pricing)). Composio hosts Gmail, SendGrid and MailSlurp MCPs ([Composio](https://composio.dev/toolkits/sendgrid)) | — | — | No |

### Table B — Commercial: pricing, funding, launch, traction, limitations

| Product | Free tier | Paid price points | Funding / backing | Launch / key dates | Traction signals | Notable limitations |
|---|---|---|---|---|---|---|
| **AgentMail** | 3 inboxes, 3,000 emails/mo, 100/day, no custom domains ([usecarly](https://www.usecarly.com/blog/agentmail-pricing/)) | Developer $20/mo: 10 inboxes, 10k emails, 10 domains. Startup $200/mo: 150 inboxes, 150k emails, 150 domains, SOC 2 report, dedicated IPs on request. Enterprise custom. ~20% off annual ([usecarly](https://www.usecarly.com/blog/agentmail-pricing/), [bavimail](https://bavimail.com/blog/agentmail-pricing-review-2026)). Extra inbox $2/mo past 10 on Developer ([deadsimple](https://deadsimple.email/compare/pricing.html)) | $6M seed led by General Catalyst, with YC, Phosphor Capital, and angels including Paul Graham and Dharmesh Shah ([TechCrunch](https://techcrunch.com/2026/03/10/agentmail-raises-6m-to-build-an-email-service-for-ai-agents/), [AgentMail blog](https://www.agentmail.to/blog/agentmail-seed-launch)) | YC S25. Show HN ~mid-2025 ([HN](https://news.ycombinator.com/item?id=44745820)); Launch HN ([HN](https://news.ycombinator.com/item?id=46812608)); seed and agent self-signup "onboarding API" 10 Mar 2026 ([TechCrunch](https://techcrunch.com/2026/03/10/agentmail-raises-6m-to-build-an-email-service-for-ai-agents/)) | Claims tens of thousands of human users, hundreds of thousands of agent users, and 500+ B2B customers ([TNW](https://thenextweb.com/news/agentmail-raises-6m-seed-ai-agent-email-inboxes)). GitHub: toolkit 111★, python SDK 73★, MCP 68★, Rust CLI 41★ ([GitHub org](https://github.com/agentmail-to)) | Price jumps 10x from $20 to $200 with no middle tier ([bavimail](https://bavimail.com/blog/agentmail-pricing-review-2026)). The shared @agentmail.to domain has shared reputation ([AgentMail KB](https://www.agentmail.to/docs/knowledge-base/emails-going-to-spam)). Some recipients filter agent domains to Junk ([coryd.dev](https://www.coryd.dev/posts/2026/agent-email-is-spam)) |
| **Cloudflare Email Service** | Inbound routing free. Sends to verified addresses in your own account are free ([whichdevtool](https://whichdevtool.com/tools/cloudflare-email-service/)) | Workers Paid $5/mo includes 3,000 sends, then $0.35 per 1,000 ([whichdevtool](https://whichdevtool.com/tools/cloudflare-email-service/), [lushbinary](https://lushbinary.com/blog/cloudflare-email-service-vs-aws-ses-agent-email-guide/)). Storage (R2/D1/DO) billed separately | Cloudflare (public company) | Agents SDK email integration 5 Aug 2025 ([CF changelog](https://developers.cloudflare.com/changelog/post/2025-08-05-agents-MCP-update/)). Sending private beta ~Sept 2025, public beta 16 Apr 2026 ([CF blog](https://blog.cloudflare.com/email-for-agents/)) | Agentic Inbox: ~8.3k★, 1.1k forks ([GitHub](https://github.com/cloudflare/agentic-inbox)) | Locked to Cloudflare DNS. Beta pricing labelled provisional. No inbox resource in the API; you build storage yourself ([usecarly](https://www.usecarly.com/blog/cloudflare-email-service/)). Agentic Inbox has **no per-mailbox authorization**: one Access policy is the only trust boundary ([GitHub](https://github.com/cloudflare/agentic-inbox)) |
| **Resend** | 3,000 emails/mo, 100/day ([usecarly](https://www.usecarly.com/blog/resend-pricing/)) | Pro $20/mo for 50k; $90 tier for 100k; overage $0.90 per 1k; received mail counts against the same quota ([usecarly](https://www.usecarly.com/blog/resend-pricing/), [automationatlas](https://automationatlas.io/answers/resend-pricing-explained-2026/)). Inboxes pricing unpublished ([soleur #9459](https://github.com/jikig-ai/soleur/issues/9459)) | VC-backed (round details not verified this session) | First-party MCP ~Apr 2026 per third-party reviews ([usecarly](https://www.usecarly.com/blog/resend-mcp/)). Inboxes private beta as of Oct 2026 ([soleur #9459](https://github.com/jikig-ai/soleur/issues/9459)) | resend-mcp 576★, 90 forks ([GitHub](https://github.com/resend/resend-mcp)) | Inboxes is pre-GA: API may change; no per-inbox retention setting; 7-day backups; **US-only storage** ([soleur #9459](https://github.com/jikig-ai/soleur/issues/9459)) |
| **Postmark** | 100 emails/mo, no inbound ([klymentiev](https://klymentiev.com/blog/postmark-pricing)) | Basic $15 for 10k (no inbound); Pro $16.50 (inbound, 10 domains); Platform $18 (inbound, unlimited domains). Pro/Platform prices cut 4 Feb 2026 ([klymentiev](https://klymentiev.com/blog/postmark-pricing), [costbench](https://costbench.com/changelog/postmark-price-decrease-2026-02-3/)) | ActiveCampaign, since May 2022 ([smtpedia](https://smtpedia.com/platforms/postmark/pricing/)) | MCP went from 4 to 24 tools in about a year ([Postmark blog](https://postmarkapp.com/blog/the-postmark-mcp-server-one-year-later-from-4-tools-to-24)) | postmark-mcp 57★ ([GitHub](https://github.com/ActiveCampaign/postmark-mcp)) | MCP is send-only. A malicious npm impersonator of the MCP package was reported ([ChatForest](https://chatforest.com/reviews/postmark-mcp-server/)) |
| **Mailgun** | 100 emails/day ([surecontact](https://clone.surecontact.com/smtp-pricing-2026/)) | From $15/mo for 10k ([surecontact](https://clone.surecontact.com/smtp-pricing-2026/)) | — | — | mailgun-mcp-server 62★ ([GitHub](https://github.com/mailgun/mailgun-mcp-server)) | MCP is local stdio only; no hosted option |
| **SendGrid** | Permanent free plan replaced by a 60-day trial in 2025 ([surecontact](https://clone.surecontact.com/smtp-pricing-2026/)) | Essentials $19.95 for 50k; Pro $89.95 for a dedicated IP ([surecontact](https://clone.surecontact.com/smtp-pricing-2026/)) | Twilio. Email merged into twilio.com Feb 2026 ([surecontact](https://clone.surecontact.com/smtp-pricing-2026/)) | — | — | No official actionable MCP; Inbound Parse needs your own MX + endpoint |
| **Amazon SES Mail Manager** | — | About $0.15 per 1k messages processed; $0.09 per 1k 256KB chunks ("Send to Internet"); **$50/mo per Open Ingress Endpoint** on Essentials/Pro, billed even when closed ([AWS pricing](https://aws.amazon.com/ses/pricing/), [Vantage](https://cur.vantage.sh/aws/amazonses/)) | AWS | Agent skills for AWS MCP Server Sept 2026 ([AWS](https://aws.amazon.com/about-aws/whats-new/2026/09/aws-messaging-ses-ai-skills-mcp-server/)) | — | Requires an AWS account and IAM setup; MCP is general-purpose, not inbox-shaped ([Nitrosend](https://nitrosend.com/best-email-api-for-ai-agents)) |
| **Mailtrap** | 4,000 emails/mo (150/day); inbound shares the quota ([remio](https://www.remio.ai/post/best-inbound-email-api-for-ai-agents-mailtrap-vs-agentmail-vs-postmark)) | Basic $15, Business $85, Enterprise from $750 ([remio](https://www.remio.ai/post/best-inbound-email-api-for-ai-agents-mailtrap-vs-agentmail-vs-postmark)) | — | Inbound GA 2 Jun 2026 ([changelog](https://feedback.mailtrap.io/changelog/inbound-is-generally-available)) | — | No custom receiving domain yet |
| **Nylas Agent Accounts** | 3 accounts, 3,000 emails/mo, 3 GB ([Yahoo/launch](https://finance.yahoo.com/technology/ai/articles/nylas-launches-agent-accounts-giving-153200500.html)) | Essentials $15 (10 accounts, 10k emails); Pro $49 (50 accounts, 50k) ([Nylas CLI guide](https://cli.nylas.com/guides/email-api-pricing-models), [deadsimple](https://deadsimple.email/compare/nylas.html)). Other sources conflict | Nylas (established integration-API company; funding not verified this session) | Beta Apr 2026, GA 17 Jun 2026 ([changelog](https://developer.nylas.com/docs/changelogs/2026-06-17-agent-accounts-ga/)) | SOC 2 Type II, HIPAA, ISO 27001/27701 ([changelog](https://developer.nylas.com/docs/changelogs/2026-06-17-agent-accounts-ga/)) | A competitor reports 100 sends/account/day, 1 GB/account, 7-day retention defaults ([deadsimple](https://deadsimple.email/compare/nylas.html)) |
| **MailSlurp** | — | Pro $49.99/mo: 5,000 received, 500 external sends ([pricing](https://app.mailslurp.com/pricing/)); top plan $129.99 with unlimited inboxes but 5k received ([deadsimple](https://deadsimple.email/compare/pricing.html)) | — | — | — | Built for QA/testing; low message caps |
| **inbound.new** | None per recent sources (older page said 1,000 free webhooks) | $4/mo for 5,000 emails, unlimited mailboxes ($9 in another listing) ([itechguides](https://www.itechguides.com/products/inbound/), [deadsimple](https://deadsimple.email/blog/email-api-cost-comparison-ai-agents-2026.html)) | — | — | 359★, 45 forks, MIT ([GitHub](https://github.com/inboundemail/inbound)) | No MCP in repo |
| **Inkbox** | 3 identities ([deadsimple](https://deadsimple.email/compare/pricing.html)) | $30 Developer (10 identities + phone); $200 (100 identities) ([deadsimple](https://deadsimple.email/compare/pricing.html)) | YC S26 ([YC](https://www.ycombinator.com/companies/inkbox)) | 2026 | — | — |
| **OpenMail** | 3 inboxes, 3,000 emails ([infraforge](https://www.infraforge.ai/blog/openmail-review)) | €9 Pro (10 inboxes, 10k); €1 per extra inbox; €0.001 per extra email ([infraforge](https://www.infraforge.ai/blog/openmail-review)) | — | — | — | — |
| **Atomic Mail** | Free in open alpha, no metering ([Atomic Mail](https://atomicmail.io/atomic-mail-vs-agentmail)) | — | — | 2026 | — | Alpha |
| **Robotomail** | 1 mailbox | $19 for 10 mailboxes + 1 domain; $79 for 50; $199 for 200 ([deadsimple](https://deadsimple.email/compare/pricing.html)) | — | ~May 2026 | — | No documented MCP per one review ([lumbox](https://lumbox.co/best-email-api-for-ai-agents/)) |
| **LobsterMail** | 1,000 emails/mo | $9 Builder (10 inboxes, 5k emails, 500 outbound/day); $99 (300 inboxes, 100k) ([deadsimple](https://deadsimple.email/compare/pricing.html)) | — | — | — | Own pages conflict on inbox limits ([lobstermail](https://lobstermail.ai/blog/lobstermail-vs-agentmail)) |
| **Mails.ai** | 1 agent, 3,000 events | $20 for 5 agents; $99 for unlimited agents and 250k sends ([deadsimple](https://deadsimple.email/compare/pricing.html)) | — | — | — | — |
| **Dead Simple Email** | 5 inboxes | $5 / $29 / $99 for 500 inboxes (~$0.20/inbox) ([deadsimple](https://deadsimple.email/compare/pricing.html)) | — | — | — | Vendor authored the comparison it tops |
| **Shipmail** | 14-day trial | From $4/mo for 2 mailboxes; ~$250 for 100 ([aisotools](https://aisotools.com/pricing/shipmail), [deadsimple](https://deadsimple.email/compare/pricing.html)) | — | — | — | Per-mailbox plans, not agent-native |
| **Microsoft** | — | Agent 365 GA 1 May 2026 at $15/user/mo, or included in M365 E7 at $99/user/mo ([beri.net](https://www.beri.net/article/microsoft-agent-365-pricing-15-vs-99), [redresscompliance](https://redresscompliance.com/agent-365-licensing-guide.html)). Agent-user mailboxes need M365 E7, or Agent 365 + Entra P1/M365 E3 ([MS Learn](https://learn.microsoft.com/en-us/entra/identity/conditional-access/policy-agent-user)) | Microsoft | Agent-user mailboxes were in Frontier preview as of ~Jun 2026 ([SamExpert](https://samexpert.com/agent-365/)) | — | Provisioning can take up to 24h ([Entrabot](https://microsoft.github.io/entrabot/platform-docs/entra-agent-users/)); Graph/Exchange support for agent identities is unclear |
| **Google Gemini agents** | — | No published price; one press report says no extra charge where Gemini Enterprise is available ([dragapp](https://www.dragapp.com/blog/gemini-agent/)) | Google | Unveiled 8 Oct 2026 at Gemini at Work; private preview ([dragapp](https://www.dragapp.com/blog/gemini-agent/), [absolutegeeks](https://www.absolutegeeks.com/tech-news/google-just-gave-ai-agents-their-own-email-addresses-and-calendars/)) | — | Enterprise-only preview |
| **Unipile** | 7-day trial ([Unipile](https://www.unipile.com/it/mcp/)) | €49/mo minimum for up to 10 accounts; €5/account for 11-50 ([aitoolsatlas](https://aitoolsatlas.ai/tools/unipile/pricing)) | — | MCP 2026 | — | Connects human Gmail/Outlook/IMAP accounts, not new agent inboxes |
| **Zapier MCP** | 100 tasks/mo (shared) | Professional $19.99/mo for 750 tasks; **2 tasks per MCP tool call** ([latenode](https://latenode.com/blog/zapier-mcp-pricing)) | Zapier | — | — | Per-call metering adds up quickly for agents |

### Cited Findings (additional context)
- AgentMail's founders say they left Gmail because they hit four limits: no programmatic inbox creation, sending caps, OAuth for every inbox, and per-seat pricing ([Launch HN](https://news.ycombinator.com/item?id=46812608)).
- AgentMail splits email MCP servers into three kinds: "create-inbox" (agent-owned inboxes), "send" (transactional ESPs), and "person mailbox" (Gmail/Outlook connectors) ([AgentMail blog](https://www.agentmail.to/blog/email-mcp-servers-for-ai-agents)).
- Resend's founder advises against building on Inboxes if you need EU residency; the evaluating team ruled it out on 5 Oct 2026 ([soleur #9459](https://github.com/jikig-ai/soleur/issues/9459)).
- Cloudflare presents Agentic Inbox as "a starting point to fork". Its stack is Email Routing (in), Email Sending (out), Workers AI, R2, Agents SDK, and one Durable Object per mailbox ([CF blog](https://blog.cloudflare.com/email-for-agents/), [X/Thomas Gauvin](https://x.com/thomasgauvin/status/2044800506732683505)).
- Open-source, self-hosted "AgentMail alternatives" are appearing: Autopilot, pitched on HN as a drop-in AgentMail replacement ([HN](https://news.ycombinator.com/item?id=47785862)); dearagent.sh, on your own Cloudflare account ([dearagent.sh](https://dearagent.sh/)); and Sandeep Panda's Workers-based build ([sandeep.dev](https://sandeep.dev/blog/open-source-agentmail-alternative-cloudflare-workers)).
- In AgentMail's HN thread, a commenter asked about IMAP; the reply says AgentMail runs the IMAP+SMTP server as a managed service ([HN search summary](https://news.ycombinator.com/item?id=48225596)).

### Inferences
- **MCP tool surface:** aimx has 12 tools. Purpose-built rivals ship far more (AgentMail 43; Mailtrap 19 inbound tools; Resend 18 groups). aimx covers the core agent loop (create inbox, list, read, send, reply, mark read). It lacks drafts, labels, thread-level tools, attachment-download tools, and allowlists.
- **Transport:** aimx is stdio-only. The leaders now lead with **remote Streamable HTTP plus OAuth** (AgentMail, Resend, Nylas) and keep stdio as a compatibility bridge. Mailgun and Postmark remain stdio-only.
- **Self-hosted data:** no hosted competitor offers it. The closest analog to aimx's pitch is **Cloudflare Agentic Inbox**, which still ties you to Cloudflare DNS. Its ~8.3k stars in about six months suggest strong developer demand for a self-hostable agent inbox, much stronger than any vendor's official MCP repo (AgentMail MCP 68★, Resend 576★).
- **Port 25:** hosted vendors solve the "no inbound port 25" problem by being the MX. Cloudflare Email Routing is free and unlimited for inbound, so for aimx it is a plausible free inbound relay (an Email Worker forwarding to a local aimx). That would bring the CF-DNS lock-in.
- **Convergence:** the transactional ESPs are moving toward agent inboxes (Resend Inboxes, Mailtrap Agent Inbox) and the identity giants are moving in too (Nylas, Microsoft, Google). The standalone agent-inbox startups face commoditization, as the many sub-$1/inbox entrants show.

### Gaps
- Could not fetch the official pricing pages for AgentMail, Resend, Nylas, Cloudflare, Mailtrap, or Postmark. Figures come from search summaries and third-party pages. AgentMail's Developer daily cap is disputed (1,000/day vs none).
- Could not read HN comment threads directly (news.ycombinator.com was blocked), so HN sentiment comes from search-snippet summaries only.
- Unverified this session: funding for Resend, Nylas, Mailtrap, Mailgun/Sinch ownership, and the small startups (Mails.ai, LobsterMail, Robotomail, OpenMail, Dead Simple, Bavimail, Lumbox).
- Nylas Agent Account pricing conflicts across sources. One unattributed summary claimed $15/mo for 20 accounts with $0.20/account overage.
- Composio pricing (summaries claimed Free / $29 / $229 tiers) could not be tied to a specific source URL.
- Atomic Mail's Product Hunt ranking (reported as #2 Product of the Day, June 2026) could not be tied to a primary source.
- No official MCP tool list was retrievable for Nylas or Cloudflare's Email endpoints.

## Q2. What does each charge per inbox / per message, and how does it scale?

### Takeaway
Two pricing models dominate. **Agent-inbox vendors charge per inbox** (about $0.20 to $2 per inbox per month at scale) plus a monthly message allowance. **ESPs and platforms charge per message**: about $0.35 to $1.80 per 1,000 sends, with inbound either free (Cloudflare) or counted against the send quota (Resend, Mailtrap). Free tiers cluster at 1 to 5 inboxes and 1,000 to 4,000 emails per month. For a 100-inbox fleet, vendor-reported monthly costs range from about $29 to $250, with Google Workspace around $700.

### Cited Findings
- **100-inbox cost (Dead Simple Email's table, read from vendor pages on 16 Sep 2026; vendor-authored):** Dead Simple $29; Bavimail $50; Lumbox, LobsterMail, Mails.ai $99; Robotomail $199; AgentMail, Inkbox $200; Shipmail $250; Google Workspace ~$700 ([deadsimple](https://deadsimple.email/compare/pricing.html)). OpenMail is about €189 at 100 inboxes ([deadsimple](https://deadsimple.email/compare/pricing.html)).
- **Per-inbox at top published tier:** Dead Simple ~$0.20; LobsterMail ~$0.33; Robotomail ~$1.00 ([deadsimple](https://deadsimple.email/compare/pricing.html)). AgentMail Startup works out to $200 / 150 inboxes ≈ $1.33 per inbox ([usecarly](https://www.usecarly.com/blog/agentmail-pricing/)).
- **AgentMail scaling:** Free (3) → $20 (10) → $200 (150) → Enterprise. A 10x jump with no middle tier ([bavimail](https://bavimail.com/blog/agentmail-pricing-review-2026)); $2 per extra inbox on Developer ([deadsimple](https://deadsimple.email/compare/pricing.html)).
- **OpenMail:** €9 for 10 inboxes, then €1 per inbox and €0.001 per email ([infraforge](https://www.infraforge.ai/blog/openmail-review)).
- **Per-message platforms:**
  - Cloudflare: $5/mo Workers Paid includes 3,000 sends, then $0.35 per 1k; inbound free ([whichdevtool](https://whichdevtool.com/tools/cloudflare-email-service/), [lushbinary](https://lushbinary.com/blog/cloudflare-email-service-vs-aws-ses-agent-email-guide/)).
  - Resend: Pro $20 for 50k, overage $0.90 per 1k; inbound counts against the quota ([usecarly](https://www.usecarly.com/blog/resend-pricing/)).
  - Postmark: overage $1.80 / $1.30 / $1.20 per 1k by tier; inbound only from $16.50 Pro ([klymentiev](https://klymentiev.com/blog/postmark-pricing)).
  - SES Mail Manager: $0.15 per 1k messages, plus $50/mo per open ingress endpoint ([AWS pricing](https://aws.amazon.com/ses/pricing/)).
- **Identity platforms:**
  - Nylas: free 3 accounts; Essentials $15 for 10; Pro $49 for 50 ([Nylas CLI guide](https://cli.nylas.com/guides/email-api-pricing-models)).
  - Microsoft: Agent 365 at $15 per *user* per month, covering every agent that user owns, plus M365 base licensing for the mailbox ([redresscompliance](https://redresscompliance.com/agent-365-licensing-guide.html), [MS Learn](https://learn.microsoft.com/en-us/entra/identity/conditional-access/policy-agent-user)).
  - Unipile: €49 minimum for 10 accounts, then €5 per account ([aitoolsatlas](https://aitoolsatlas.ai/tools/unipile/pricing)).
- **Metered-connector cost:** Zapier MCP spends 2 tasks per tool call; a 20-step agent job spends 40 tasks ([latenode](https://latenode.com/blog/zapier-mcp-pricing)).
- **Testing-oriented:** MailSlurp Pro is $49.99 for 5,000 received and 500 external sends ([MailSlurp](https://app.mailslurp.com/pricing/)).

### Inferences
- A self-hosted aimx has zero marginal cost per inbox. Against a market where 100 agent inboxes run $29 to $250/month and enterprise seats run ~$7/inbox, that is a clear price pitch.
- If aimx offers a hosted inbound relay (for users without port 25), market rates suggest a price range of about $0.20 to $1 per inbox per month, or $3 to $10/month flat for hobbyists. Cloudflare's free inbound sets the floor at $0 for the routing part alone.

### Gaps
- Official per-message overage rates for most agent-inbox startups were not verified.
- SES outbound per-message price was not verified in this session.
- Storage and retention limits per tier are mostly undocumented in the sources found (exception: Nylas 3 GB free).

## Q3. Which products target "give each agent its own inbox" vs. "send transactional email"?

### Takeaway
"Own inbox per agent" is the explicit pitch of AgentMail, Nylas Agent Accounts, Inkbox, OpenMail, Atomic Mail, Robotomail, LobsterMail, Mails.ai, Dead Simple Email, Lindy Mail, Microsoft agent users, Google Gemini agents, and Cloudflare Agentic Inbox. Resend (Inboxes beta) and Mailtrap (Agent Inbox) are crossing over from transactional. Postmark, Mailgun, SendGrid, and SES remain transactional-first: their MCPs mostly send and manage, and inbound is a webhook or rule you wire up yourself.

### Cited Findings
**Agent-inbox-first:**
- AgentMail: "API that gives agents their own email inboxes"; agents can create inboxes for themselves via the onboarding API ([HN](https://news.ycombinator.com/item?id=46812608), [TechCrunch](https://techcrunch.com/2026/03/10/agentmail-raises-6m-to-build-an-email-service-for-ai-agents/)).
- Nylas: "a real name@yourdomain.com mailbox and calendar that an AI agent owns end to end" ([Nylas changelog](https://developer.nylas.com/docs/changelogs/2026-06-17-agent-accounts-ga/)).
- Inkbox: "identity and communication layer for AI agents" covering email, phone, iMessage, and a credential vault ([YC](https://www.ycombinator.com/companies/inkbox)).
- Atomic Mail: agents self-register via proof-of-work ([Atomic Mail](https://atomicmail.io/atomic-mail-vs-agentmail)).
- AgentEmail: "email infrastructure designed exclusively for AI agents as the end-user" ([hunted.space](https://hunted.space/product/agentemail)).

**Enterprise:**
- Microsoft agent users exist for agents "that need a mailbox, chat access, or a team-member role" ([MS Learn](https://learn.microsoft.com/en-us/entra/identity/conditional-access/policy-agent-user)).
- Google's coworker agents get an `@agents` address and log actions to the agent, not a person ([dragapp](https://www.dragapp.com/blog/gemini-agent/)).

**Crossovers:**
- Resend's MCP Inboxes group covers create/list/update inboxes, agent settings, threads, drafts, and labels ([GitHub](https://github.com/resend/resend-mcp)). A CLI PR adds agent instructions, tone, and allowed-actions settings per inbox ([resend-cli PR](https://github.com/resend/resend-cli/pull/401)).
- Mailtrap's Agent Inbox lets an agent create inboxes, read inbound mail, reply in-thread, and manage webhooks via MCP ([Mailtrap](https://mailtrap.io/agent-inbox/)).

**Transactional-first:**
- Postmark's MCP has 24 tools and none read inbound mail ([GitHub](https://github.com/ActiveCampaign/postmark-mcp)).
- Mailgun's MCP can view and update inbound Routes but not read mail as an inbox ([GitHub](https://github.com/mailgun/mailgun-mcp-server)).
- SendGrid Inbound Parse requires your own MX and receiving endpoint ([Twilio docs](https://www.twilio.com/docs/sendgrid/for-developers/parsing-email/setting-up-the-inbound-parse-webhook), [Scalekit](https://www.scalekit.com/blog/sendgrid-mcp-vs-api)).

**Connectors to human mailboxes (neither category):** Unipile (Gmail/Outlook/IMAP) ([aitoolsatlas](https://aitoolsatlas.ai/tools/unipile/pricing)), Zapier MCP, Composio Gmail ([AgentMail blog taxonomy](https://www.agentmail.to/blog/email-mcp-servers-for-ai-agents)).

### Inferences
- aimx is squarely in the "own inbox per agent" camp: mailbox CRUD over MCP, per-mailbox trust, and hooks. Its feature set matches AgentMail's core.
- aimx's distinct angles in that camp:
  - **Local plain-text storage** (Markdown + TOML) that agents can read with file tools, no API needed.
  - **Shell hooks** on receive and send.
  - **OS-user ownership** of mailboxes.
  - **No vendor in the data path.**

  No hosted competitor offers file-system-native storage.

### Gaps
- Could not verify whether Nylas or AgentMail offer per-inbox trust or sender-allowlist gating equivalent to aimx's `trust` / `trusted_senders`. AgentMail does document allow/blocklists ([docs](https://docs.agentmail.to/knowledge-base/inbox-capabilities)).

## Q4. What gaps do users complain about (privacy, cost, lock-in, latency, deliverability, no self-hosting)?

### Takeaway
The recurring complaints are:
- **Data residency and privacy:** US-only storage, and the vendor becomes the system of record for third-party mail.
- **Price cliffs:** per-inbox pricing.
- **Deliverability and abuse:** shared agent domains get filtered, and spam signups are an "infinite whack-a-mole".
- **Platform lock-in:** Cloudflare DNS requirement; Microsoft and Google licensing.
- **Pre-GA instability:** Resend Inboxes, Cloudflare beta, Microsoft Frontier preview.
- **Thin MCP coverage:** many ESP MCPs can't read inbound mail.

A wave of open-source, self-hosted "AgentMail alternatives" in 2026 is direct evidence that people want to self-host.

### Cited Findings
- **Privacy / residency:**
  - Resend stores all Inboxes data (bodies, headers, attachments, logs, backups) in the US and cannot commit to EU-only storage. An evaluating team ruled it out for EU users, noting it would make Resend "the system of record for full third-party mail" ([soleur #9459](https://github.com/jikig-ai/soleur/issues/9459)).
  - OpenMail markets EU residency on every plan as its differentiator against AgentMail ([OpenMail](https://openmail.sh/compare/agentmail-alternative)).
- **Cost:**
  - AgentMail's "$20 to $200 cliff" ([bavimail](https://bavimail.com/blog/agentmail-pricing-review-2026)); LobsterMail's "the gap nobody warns you about" post ([lobstermail](https://lobstermail.ai/blog/agentmail-pricing-review-2026-the-gap-nobody-warns-you-about)).
  - Autopilot's HN pitch: self-hosted is "effectively free" versus $20/mo for 10 inboxes ([HN](https://news.ycombinator.com/item?id=47785862)).
  - Unipile's €49 minimum is called high for prototypes ([aitoolsatlas](https://aitoolsatlas.ai/tools/unipile/pricing)).
  - Zapier MCP's 2-tasks-per-call metering ([latenode](https://latenode.com/blog/zapier-mcp-pricing)).
- **Deliverability / abuse:**
  - HN commenters predicted "a ton of abusers… signing up to services, spamming… which then blacklists the domain. This is an infinite whack-a-mole" ([HN](https://news.ycombinator.com/item?id=46812608)).
  - AgentMail itself warns that the shared @agentmail.to domain has shared reputation ([AgentMail KB](https://www.agentmail.to/docs/knowledge-base/emails-going-to-spam)).
  - A blogger publishes a Sieve rule that sends mail from agentmail.to and other agent-email domains straight to Junk: "agent email lives at the intersection of slop and spam" ([coryd.dev](https://www.coryd.dev/posts/2026/agent-email-is-spam)).
  - Shared IPs are the default at AgentMail; dedicated IPs come only on request at higher tiers ([inboxkit](https://www.inboxkit.com/learn/agentmail-review)).
- **Lock-in / platform requirements:**
  - Cloudflare Email Routing needs the domain on Cloudflare DNS ([lushbinary](https://lushbinary.com/blog/cloudflare-email-service-vs-aws-ses-agent-email-guide/)).
  - Microsoft agent mailboxes need E7, or Agent 365 + P1/E3, and can take up to 24h to provision ([MS Learn](https://learn.microsoft.com/en-us/entra/identity/conditional-access/policy-agent-user), [Entrabot](https://microsoft.github.io/entrabot/platform-docs/entra-agent-users/)).
  - SES Mail Manager charges $50/mo per open ingress endpoint even when it is closed ([AWS pricing](https://aws.amazon.com/ses/pricing/), [Vantage](https://cur.vantage.sh/aws/amazonses/)).
- **Moat / defensibility:** an HN commenter asked AgentMail "what's the plan if someone does what you do, faster and cheaper?" ([HN](https://news.ycombinator.com/item?id=46812608)).
- **Maturity:**
  - Resend Inboxes: preview SDK, API may change, no retention policy yet ([soleur #9459](https://github.com/jikig-ai/soleur/issues/9459)).
  - Cloudflare's per-message rate is provisional during beta ([usecarly](https://www.usecarly.com/blog/cloudflare-email-service/)).
  - Mailtrap has no custom receiving domain yet ([changelog](https://feedback.mailtrap.io/changelog/inbound-is-generally-available)).
  - Microsoft agent identities got 401s calling Exchange via Graph ([MS Q&A](https://learn.microsoft.com/en-us/answers/questions/5951873/do-entra-agent-id-agent-identities-support-exchang)).
- **Security footguns:**
  - Cloudflare Agentic Inbox has no per-mailbox authorization; anyone who passes Access can reach every mailbox, including via MCP ([GitHub](https://github.com/cloudflare/agentic-inbox)).
  - A malicious npm package impersonated the Postmark MCP ([ChatForest](https://chatforest.com/reviews/postmark-mcp-server/)).
- **Self-hosting demand:** Agentic Inbox at ~8.3k★ ([GitHub](https://github.com/cloudflare/agentic-inbox)); dearagent.sh ([site](https://dearagent.sh/)); Autopilot ([HN](https://news.ycombinator.com/item?id=47785862)); Sandeep Panda's open-source AgentMail alternative ([sandeep.dev](https://sandeep.dev/blog/open-source-agentmail-alternative-cloudflare-workers)).
- **Self-hosting pain (older, general):** HN commenters note that self-hosters "can reliably receive email, but can't reliably send it" ([HN, 2022](https://news.ycombinator.com/item?id=32720234)). This is the outbound-deliverability risk aimx's direct-to-MX design inherits.

### Inferences
- aimx's strongest wedges against hosted rivals:
  - **Data sovereignty:** mail never leaves the user's machine or VPS, which answers the Resend/EU complaint directly.
  - **No per-inbox cost.**
  - **No platform lock-in:** unlike Agentic Inbox, no Cloudflare DNS requirement.
  - **Per-mailbox OS-user authz:** aimx uses `SO_PEERCRED`; Agentic Inbox lacks per-mailbox authorization.
- aimx's biggest exposures, the same ones that hurt self-hosted email generally:
  - **Outbound deliverability** from residential or new IPs. Desktop installs on macOS/Windows usually have no PTR record and are on residential blocklists.
  - **No inbound port 25** on home and cloud networks.

  Hosted vendors win on exactly these two points. An optional relay (inbound MX relay plus outbound smarthost) would neutralize them while keeping data local.
- Abuse and reputation backlash against "agent email domains" (the coryd.dev Junk rule, the HN whack-a-mole comment) favors users sending from their *own* domain and IP, as aimx encourages, over shared vendor domains. A future aimx relay service would inherit this reputation-management burden.

### Gaps
- No Reddit threads were found by search. Community-complaint evidence is limited to HN snippets, GitHub issues, and blogs.
- Couldn't measure latency complaints (webhook or delivery delay) for any vendor; no sources found.
- No public DNSBL listing for agentmail.to was found; the deliverability complaints are anecdotal.
