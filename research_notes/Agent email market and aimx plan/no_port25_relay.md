# Running aimx without port 25: relay design landscape (as of 2026-10-16)

Research method note: in this session WebFetch could not resolve any hostname and direct curl to doc sites was refused by the egress proxy, so every finding below comes from WebSearch result pages (titles, URLs and the search tool's extracted snippets). Where the only available source is secondary (vendor blog, aggregator, forum), that is stated. The WebSearch budget ran out before several follow-ups (listed under Gaps). Today's date is 2026-10-16. "Dated" means the source is older than about 12 months and may be stale.

aimx context checked in code: `src/ingest.rs` calls `verify_auth(raw, received_from_ip, envelope_mail_from)` and checks SPF against the SMTP peer IP plus envelope MAIL FROM using the `mail-auth` crate (`src/ingest.rs` lines ~308 and ~777-920). So any relay that sits in front of aimx changes the peer IP the node sees. That breaks SPF unless the relay passes the original client IP and envelope through, or checks auth itself.

---

## 1. Port 25 blocking facts: cloud providers, residential ISPs, CGNAT/IPv6

### Takeaway
Most big clouds block outbound TCP 25 by default as of 2026. GCP never lifts it. DigitalOcean also blocks 465/587. Azure lifts it only for Enterprise Agreement and MCA-E subscriptions. AWS, Hetzner, Vultr, Linode and Oracle lift it on request after review. Residential lines face two problems: ISPs block port 25, and end-user IP ranges are on Spamhaus PBL. Fixing the port alone would still leave the IP blocked for direct-to-MX sending, so a relay is the realistic path for laptops and home machines.

### Cited Findings

**Cloud providers (outbound 25)**
- **AWS:** outbound port 25 is blocked by default for EC2 instances, and for Lambda functions on elastic network interfaces, unless the account is allowlisted. Removal goes through an EC2 request form, one request per Region. Lambda must first be attached to a VPC with NAT internet access. AWS recommends SES because email providers may block cloud IP ranges — [AWS re:Post knowledge center: ec2-port-25-throttle](https://repost.aws/knowledge-center/ec2-port-25-throttle). Lightsail has its own equivalent article — [AWS re:Post: lightsail-port-25-throttle](https://www.repost.aws/knowledge-center/lightsail-port-25-throttle).
- **AWS:** community answers say 587 and 465 are not restricted. Some users report requests rejected even with long-established domains (anecdotal) — [AWS re:Post question](https://repost.aws/questions/QUhOiYfU2ESzyjHNFLdih5lQ/smtp-port-25-outgoing-connections-blocked).
- **Google Cloud (GCE):** port 25 is "always blocked and cannot be used." Google's stated reason is "the large amount of abuse server-to-server SMTP is susceptible to" — [Cameyo help center](https://helpcenter.cameyo.com/support/solutions/articles/80000212536-sending-email-from-a-cameyo-hosted-server). GCE "does not allow outbound connections from [port 25] under any circumstances"; 587/465 are suggested for relay — [Plesk KB](https://www.plesk.com/kb/support/is-it-possible-to-send-email-directly-from-a-plesk-server-hosted-on-google-cloud/).
  - Conflict: a 2017 forum post quotes older Google docs saying 25, 465 *and* 587 were all blocked — [Virtualmin archive (dated 2017)](https://archive.virtualmin.com/node/53820). Google's own doc was not reachable this session.
- **Azure:** outbound TCP 25 is *not* blocked for VMs in standard Enterprise Agreement or MCA-E subscriptions, though Microsoft gives no guarantee that receivers will accept the mail. It is blocked by default on Enterprise Dev/Test, which can request removal via "Diagnose and Solve." It is blocked on all other subscription types. App Service and Functions can use 25 via VNet integration or ASE v3; other PaaS cannot. Microsoft recommends authenticated relays on 587 — [Microsoft Learn: Troubleshoot outbound SMTP connectivity](https://learn.microsoft.com/hr-hr/azure/virtual-network/troubleshoot-outbound-smtp-connectivity); [Azure China mirror](https://docs.azure.cn/en-us/virtual-network/troubleshoot-outbound-smtp-connectivity).
  - A March 2025 Q&A moderator (community, not official) says Pay-As-You-Go, Free Trial, Sponsorship and Students will not be unblocked even on request. A June 2025 Microsoft staff reply called the block expected for non-enterprise subscriptions — [Microsoft Q&A](https://learn.microsoft.com/en-us/answers/questions/2259831/smtp-port-25-is-blocked-need-to-unblock); [Microsoft Q&A](https://learn.microsoft.com/en-us/answers/questions/2238133/allow-outbound-port-25).
- **DigitalOcean:** official docs (last verified 2026-07-13) say SMTP ports 25, 465 *and* 587 are blocked on Droplets, including traffic via Reserved IPs. DO recommends a third-party email provider — [DigitalOcean docs: Why is SMTP blocked?](https://docs.digitalocean.com/support/why-is-smtp-blocked/).
  - Staff replies on unblocking conflict: one says wait 60 days, another says the block "can't be removed" — [DO community](https://www.digitalocean.com/community/questions/25-port-block-for-all-news-users); [DO community](https://www.digitalocean.com/community/questions/is-port-25-blocked-by-digital-ocean-preventing-postfix-sending-emails).
  - A third-party guide claims 2525 is open for relaying (unverified) — [findstack](https://findstack.com/resources/port-25-blocked-vps).
- **Hetzner Cloud:** ports 25 and 465 are blocked by default on all cloud servers. After one month as a customer and a paid first invoice, you can file a limit request with your use case. Port 587 is open. This is the FAQ wording as quoted secondhand — [Discourse Meta thread quoting Hetzner](https://meta.discourse.org/t/ports-blocked-hetzner-cloud-server/225146?tl=en); [findstack](https://findstack.com/resources/port-25-blocked-vps).
  - Users report support asking about mail type and volume. Some requests were refused with a "use a bulk mail provider" reply — [sudonix](https://sudonix.org/post/8489).
- **Vultr:** outbound 25 is blocked by default on new accounts and instances. You can request unblocking by support ticket, stating your use case, SPF/DKIM/rate limits and volume. Approval is case-by-case, and Vultr recommends 465/587 relays — [Vultr Docs: Why is SMTP blocked](https://docs.vultr.com/support/products/compute/why-is-smtp-blocked).
  - Anecdotes conflict on whether one unblock covers the whole account or each instance — [LowEndTalk](https://lowendtalk.com/discussion/comment/3569213/); [BitLaunch help](https://help.bitlaunch.io/en/articles/4851742-emailing-on-our-services).
- **Linode/Akamai:** ports 25, 465 and 587 are restricted on Compute Instances "for some new accounts created after November 5th, 2019" — [Akamai TechDocs: Send email](https://techdocs.akamai.com/cloud-computing/docs/send-email).
  - The block is lifted by ticket after you set forward and reverse DNS and confirm CAN-SPAM compliance. AUP violations can reinstate it — [Linode community](https://www.linode.com/community/questions/20849/ask-port-25465587).
- **Oracle Cloud (OCI):** tenancies created after 2021-06-23 cannot send on outbound TCP 25 by default. Exemption is by service limits request — [OCI release note](https://docs.oracle.com/en-us/iaas/releasenotes/changes/f7e95770-9844-43db-916c-6ccbaf2cfe24). Community reports say 587 works with OCI Email Delivery — [HestiaCP forum](https://forum.hestiacp.com/t/oracle-cloud-email-as-relay-doesnt-works/11304).
- **Fly.io:** a staff reply says there is "no per-account blocking of outbound port 25." However, Fly offers no customer-configurable PTR records, which large receivers may reject. Inbound SMTP needs a dedicated IPv4, because the shared-IPv4 proxy can't route plain SMTP — [Fly community](https://community.fly.io/t/outbound-port-25-direct-to-mx-smtp-blocked-by-default-on-new-accounts/28537); [Fly community: inbound SMTP](https://community.fly.io/t/inbound-smtp-server-ports-25-465-587-on-a-fly-app/28000).
  - One user found Postmark SMTP timed out from Fly but the Postmark HTTPS API worked (user diagnosis) — [Fly community](https://community.fly.io/t/smtp-outbound-timeout/25250).
- **Others (secondary only):** OVH is listed as port 25 "open by default" with configurable rDNS. Contabo is "open but limited to 25 emails per minute" — [forwardemail/awesome-mail-server-providers](https://github.com/forwardemail/awesome-mail-server-providers); [mailflowauthority comparison](https://mailflowauthority.com/self-hosted-smtp/best-vps-email-server). IONOS has an unblocking procedure — [IONOS help](https://www.ionos.com/help/server-cloud-infrastructure/firewall-policies/unblocking-port-25-for-sending-emails/).

**Residential ISPs**
- Comcast Xfinity does not support port 25 for residential customers' email. It recommends 587 with TLS and cites industry guidance that port 25 should be blocked — [Xfinity support](https://www.xfinity.com/support/internet/email-port-25-no-longer-supported/).
- Historical (dated 2009): NANOG posters disputed whether Comcast, Verizon and AT&T filtered port 25. John Levine said all three did, though not on every line. Small ISPs commonly blocked dynamic IPs but not static ones — [NANOG 2009](https://mailman.nanog.org/pipermail/nanog/2009-June/011386.html).
- Spamhaus PBL is "a dataset containing end-user IP address ranges from which email should never be sent directly to the final destination." Listings are policy-based (not triggered by spam), cover dynamic and static ranges and some IPv6. Spamhaus expects end users to submit via an authenticated SMTP server run by the ISP or an external service — [Spamhaus PBL](https://www.spamhaus.org/blocklists/policy-blocklist); [Spamhaus PBL FAQ](https://www.spamhaus.org/faq/section/Spamhaus-PBL).
  - Delisting by end users often fails while the upstream ISP still flags the range (secondary) — [Suped](https://www.suped.com/knowledge/email-deliverability/troubleshooting/how-to-remove-ip-address-from-spamhaus-pbl-list).
- Home-server write-ups: "most ISPs block port 25. In addition to that, residential IP addresses are generally blacklisted" — [mynacol blog](https://blog.mynacol.xyz/en/mail-server-at-home/).

**CGNAT / IPv6**
- A 2017 study used 5,121 vantage points across more than 60 European and US ISPs. It found about 10% of the ISPs tested had some CGN deployment. This counts ISPs, not subscribers, and is dated — [Mandalari et al., "Tracking the Big NAT" (arXiv 1704.01296)](https://arxiv.org/pdf/1704.01296); [CAIDA](https://www-old.caida.org/publications/papers/2017/tracking_big_nat_across/).
- Cloudflare research (reported Nov 2025): carriers in Africa and Asia use CGNAT more than carriers elsewhere, and shared IPs cause collateral blocking — [The Register, 2025-11-03](https://www.theregister.com/2025/11/03/cloudflare_cgnat_bias_research/).
- Gmail's all-sender rules require a PTR record that resolves to a hostname, and that hostname must have an A (IPv4) or AAAA (IPv6) record back to the same IP — [Google: Email sender guidelines](https://support.google.com/a/answer/81126?hl=en-GB).
  - Practitioners have long advised against sending to Gmail over IPv6 (dated 2020) — [Spam Resource](https://www.spamresource.com/2020/11/honestly-dont-send-to-gmail-over-ipv6.html). Gmail returns 550 5.7.26 for IPv6 senders without PTR or auth — [bentonow](https://bentonow.com/smtp-errors/gmail/550-5-7-26).

### Inferences
- aimx running on Mac, Windows or Linux desktops will almost never have usable inbound or outbound 25. Even when outbound 25 is open, a residential IP is likely on Spamhaus PBL and lacks a matching PTR. That fails Gmail's all-sender PTR rule. So "unblock port 25" is not a real fix for home users. Outbound must leave from a relay IP with proper FCrDNS.
- Inbound needs a public IPv4 with TCP 25 reachable. CGNAT, laptop mobility (sleep, roaming Wi-Fi) and dynamic IPs make that impossible or unreliable at home. The node must initiate the connection outbound.
- DigitalOcean blocks 587 too, and Fly users have seen relay SMTP time out. A relay design that tunnels both inbound and outbound mail over HTTPS/WSS on port 443 avoids every SMTP-port block in the table above. Relying on 587/2525 submission would fail on DO and some corporate networks.
- For the self-host-relay path (user rents a VPS), the realistic cheap options are providers that unblock after a waiting period (Hetzner after 1 month plus an invoice, Vultr or Linode by ticket) or are reportedly open (OVH). GCP, DO and non-EA Azure are effectively ruled out. Fly.io lacks PTR and is ruled out.
- The aimx verifier already probes whether port 25 is reachable. It can be extended into a mode selector: "direct" (inbound 25 reachable, outbound 25 open, PTR OK, not on PBL) vs "relay" (anything else). Ideally it would also check PBL and PTR, not just TCP reachability.

### Gaps
- Primary-source wording for Hetzner and GCP port policies was not retrievable (proxy refused the doc hosts). The Hetzner wording here is quoted secondhand.
- No current (2025-2026) data on the share of residential subscribers with outbound or inbound 25 blocked, or behind CGNAT. Only the 2017 ISP-level figure and Cloudflare's qualitative 2025 finding.
- No current AT&T, Verizon or EU/Asia ISP port-25 policy pages were found.
- IPv6-only MX viability (how many sending MTAs still can't reach an IPv6-only MX) was not found.

---

## 2. Inbound options without port 25 (relay+tunnel, Cloudflare Email Workers, ESP webhooks, generic TCP tunnels)

### Takeaway
MX records can't carry a port, so every inbound option needs some host listening on public TCP 25. The choice is whose host it is (ours, Cloudflare's, an ESP's, or the user's VPS) and how mail then reaches a NAT'd node. Off-the-shelf consumer tunnels mostly can't do public port 25:
- Cloudflare Tunnel TCP needs cloudflared on the *client*.
- Tailscale Funnel allows only 443/8443/10000.
- ngrok TCP assigns random ports.

The viable patterns are:
1. Our own MX relay plus an outbound-initiated tunnel or store-and-forward.
2. Cloudflare Email Routing to an Email Worker, then to the node or a queue.
3. ESP inbound (SES, Postmark, Mailgun, Resend) into a queue the node pulls from.
4. frp/rathole/WireGuard to a VPS with port 25.

### Cited Findings

**Port 25 is mandatory for MX**
- MX records give only a hostname, not a port. Sending MTAs hard-code port 25, and other servers won't deliver to non-standard ports. The standard workaround is an outside relay that accepts on 25 and forwards to your port — [ISC bind-users 2001 (dated)](https://lists.isc.org/pipermail/bind-users/2001-January/021758.html); [cPanel support](https://support.cpanel.net/hc/en-us/articles/4402368750871-Can-t-receive-emails-in-my-cPanel-server-).

**Sender retry behavior (relevant to "node offline")**
- RFC 5321 §4.5.4.1 says the give-up time should generally be at least 4-5 days, with retry intervals of at least 30 minutes. Postfix's default queue lifetime is 5 days. Some providers give up far sooner; one HN anecdote has Yahoo bouncing after about 3 hours — [HN via brianlovin.com](https://brianlovin.com/hn/30713068); [RFC 1123 text via freesoft.org](https://freesoft.org/CIE/RFC/1123/107.htm); [mailcow commit discussion](https://git.gnous.fr/Gnous/mailcow/src/commit/d5eeb3e8af18e9cee48452eae79bd142f7d24ebc).

**(a) Own relay MX with forwarding to the node**
- The common DIY pattern runs the mail stack at home and tunnels SMTP through a cheap VPS over WireGuard, optionally with FRP for IMAP/POP — [GitHub gist: Mailcow + WireGuard VPS](https://gist.github.com/sussycatgirl/4bcd6508dedd21ea21ec494499c92618). Another approach DNATs 25/587/465/993 from the VPS to the home server's WireGuard address — [Lemmy thread](https://lemmy.brendan.ie/comment/910050).
- Outbound must also leave from the IP your SPF authorizes. HELO should match the VPS PTR, so default transport is routed through the tunnel — [mynacol blog](https://blog.mynacol.xyz/en/mail-server-at-home/); [Lemmy thread](https://lemmy.brendan.ie/comment/910050).
- A split design: the VPS relay handles receiving and delivery, while the home server runs Dovecot, spam filtering and DKIM signing. Port 2525 is commonly used between the two hops — [administrator.de tutorial](https://administrator.de/tutorial/mailserver-relay-selfhosted-reputation-677373.html).
- **Real client IP over a TCP relay:** frp supports PROXY protocol v1/v2 via `transport.proxyProtocolVersion`, and "any TCP service that supports this protocol can obtain the user's real IP." The local service must parse the header — [frp docs: Get Real User IP](https://gofrp.org/en/docs/features/common/realip/). SMTP servers such as aiosmtpd and nodemailer smtp-server support PROXY protocol, and the docs advise restricting it to trusted proxy IPs — [aiosmtpd docs](https://aiosmtpd.aio-libs.org/en/latest/proxyprotocol.html); [nodemailer smtp-server guide](https://mintlify.com/nodemailer/smtp-server/guides/proxy-protocol).

**(b) Cloudflare Email Routing + Email Workers**
- Email Routing allows 200 rules and 200 destination addresses per domain. Each rule maps one address to one destination address or an Email Worker. Destinations must be verified before rules activate — [Cloudflare Email Routing limits](https://developers.cloudflare.com/email-routing/limits); [Email Routing addresses](https://developers.cloudflare.com/email-routing/setup/email-routing-addresses/).
- Inbound messages over 25 MiB are not supported — [Cloudflare Email Routing limits](https://developers.cloudflare.com/email-routing/limits). Base64 makes on-wire size larger than the attachments (community explanation) — [Cloudflare community](https://community.cloudflare.com/t/cloudflare-email-forwarding/358448).
- The Worker `email()` handler receives a `ForwardableEmailMessage` with `raw` (a stream), `rawSize` and `forward()`. `forward()` only goes to verified addresses, and `raw` can be read once — [Cloudflare Workers API: email handler](https://developers.cloudflare.com/email-service/api/route-emails/email-handler/); [third-party skill doc (unofficial)](https://cdn.jsdelivr.net/npm/memory-journal-mcp@8.0.1/skills/cloudflare/references/email-workers/README.md).
- Workers CPU: 10 ms per request on the free plan; 30 s default and 5 min max on paid. Email Workers that exceed this fail with EXCEEDED_CPU — [Workers limits](https://developers.cloudflare.com/workers/platform/limits).
- Since 2025-07-03, Cloudflare requires SPF or DKIM to pass before it forwards, and strongly recommends DMARC. Sources differ on whether failing mail is dropped or forwarded as untrusted — [Cloudflare Email Routing changelog](https://developers.cloudflare.com/changelog/product/email-routing/); [Cloudflare postmaster docs](https://developers.cloudflare.com/email-routing/postmaster/); [Spam Resource 2025-07](https://www.spamresource.com/2025/07/cloudflare-now-requires-auth-for-email.html).
- Cloudflare uses ARC to preserve its SPF/DKIM results when forwarding, adding ARC-Authentication-Results. It checks alignment against the sender domain's DMARC policy — [Cloudflare blog: Email Routing subdomains, new APIs and security protocols](https://blog.cloudflare.com/email-routing-subdomains/).
- Open-source examples: a Worker that POSTs incoming mail to a webhook (0 stars, unvetted) — [nhdms/cloudflare-email-webhook-worker](https://github.com/nhdms/cloudflare-email-webhook-worker). The kill-the-news email-to-RSS project ingests via the Email Worker `email()` handler — [kill-the-news](https://git.unsupervised.ca/GitHub/kill-the-news/src/commit/59cbbd04286895441c26851b103c6fe2cfb92ba1). `postal-mime` is the usual Worker MIME parser — [flaviocopes: Cloudflare Email Workers](https://flaviocopes.com/cloudflare-email-workers/).

**(c) ESP inbound (webhook or storage)**
- **Amazon SES receiving:**
  - Receipt rules can take S3, SNS, Lambda, Bounce, Stop or WorkMail actions. Notifications carry SPF, DKIM, DMARC, spam and virus verdicts — [AWS SES docs: receiving notification contents](https://docs.aws.amazon.com/ses/latest/DeveloperGuide/receiving-email-notifications-contents.html).
  - An AWS Lambda blueprint bounces mail failing these checks — [AWS Messaging blog](https://aws.amazon.com/blogs/messaging-and-targeting/introducing-the-aws-lambda-blueprint-for-filtering-emails-received-through-amazon-ses/).
  - Receiving resources other than S3 must be in the same Region as the SES endpoint — [oneuptime guide](https://oneuptime.com/blog/post/2026-02-12-use-ses-for-receiving-emails/markdown).
- **Postmark inbound:**
  - Postmark POSTs JSON to your webhook. Failed POSTs are retried 10 times over about 10.5 hours (1 min, 5 min, 3×10 min, 15, 30, 60 min, 2 h, 6 h). A 403 stops retries. After that the message shows "Inbound Error" and is missed if the endpoint stayed down. The schedule is not customizable — [Postmark support: inbound webhook retries](https://postmarkapp.com/support/article/understanding-inbound-webhook-retries-in-postmark).
  - Manual retry is possible via the API — [Postmark support](https://postmarkapp.com/support/article/1309-how-to-manually-retry-a-failed-inbound-message).
  - Inbound processing is on Pro and Platform tiers only, not Basic (secondary) — [automationatlas](https://automationatlas.io/answers/postmark-pricing-explained-2026/).
- **Resend inbound** (launched 2025-11-03):
  - Webhooks (`email.received`) carry metadata only; body and attachments come from separate API calls.
  - You can use a Resend-managed `*.resend.app` receiving domain or your own MX.
  - Resend "retains all inbound emails" if your webhook isn't ready — [Resend blog: Inbound Emails](https://resend.com/blog/inbound-emails); [Resend receiving docs](https://resend.com/docs/dashboard/receiving/).
- **Mailgun:** inbound routes are reported as 1 on Free, 5 on Basic and "full routing" on Foundation and above. Sources conflict, and some say routing is on all paid plans — [usecarly](https://www.usecarly.com/blog/mailgun-pricing/); [automationatlas](https://automationatlas.io/answers/mailgun-pricing-explained-2026/).

**(d) Generic TCP tunnels**
- **Cloudflare Tunnel:** non-HTTP (TCP) public hostnames require cloudflared on the *client* (`cloudflared access tcp`). Arbitrary TCP is streamed over WebSocket and needs cloudflared on both host and client. This makes it unusable for an MX that arbitrary internet MTAs must reach — [Cloudflare docs: Arbitrary TCP](https://developers.cloudflare.com/cloudflare-one/access-controls/applications/non-http/cloudflared-authentication/arbitrary-tcp/); [Cloudflare docs: Protocols for published applications](https://developers.cloudflare.com/cloudflare-one/networks/connectors/cloudflare-tunnel/routing-to-tunnel/protocols/); [Home Assistant community](https://community.home-assistant.io/t/cloudflared-fowarding-tcp/563636).
- **Cloudflare Spectrum** can sit in front of SMTP as a TCP pass-through, but "will not act as an intermediary mail server":
  - Spectrum apps have no reverse DNS, and Cloudflare warns of rejections. PROXY protocol is recommended for real client IPs — [Cloudflare Spectrum configuration options](https://developers.cloudflare.com/spectrum/reference/configuration-options/).
  - When the origin is behind a Cloudflare Tunnel virtual network, `proxy_protocol` must be off (so the origin loses the client IP) and the origin must be a single port.
  - Pro and Business support selected protocols only. Enterprise supports all TCP and UDP. Custom TCP apps need Enterprise with Spectrum as a paid add-on — [Cloudflare Spectrum get started](https://developers.cloudflare.com/spectrum/get-started/); [Spectrum llms-full](https://developers.cloudflare.com/spectrum/llms-full.txt).
- **Tailscale Funnel:** "Funnel can only listen on ports 443, 8443, and 10000," including raw `--tcp` forwarders. Hostnames must be under the tailnet's `ts.net` — [Tailscale docs: Funnel](https://tailscale.com/docs/features/tailscale-funnel); [tailscale funnel CLI](https://tailscale.com/docs/reference/tailscale-cli/funnel).
- **ngrok:** public TCP endpoints get "a randomly assigned port"; "you may not select the port." Reserved TCP addresses are also random host:port (e.g., `1.tcp.ngrok.io:12345`). TCP needs a payment method on file even on the free plan — [ngrok docs: TCP cloud endpoints](https://ngrok.com/docs/universal-gateway/cloud-endpoints/tcp). ngrok's SMTP guide shows `ngrok tcp 25` for exposing a *local* port 25 — [ngrok docs: SMTP](https://ngrok.com/docs/using-ngrok-with/smtp).
- **frp / rathole / bore:**
  - rathole is a Rust NAT-traversal reverse proxy, with frp-like TOML config, a mandatory token, and a binary as small as ~500 KiB. It claims more connections and less memory than frp — [rapiz1/rathole](https://github.com/rapiz1/rathole); [rathole-org/rathole](https://github.com/rathole-org/rathole).
  - bore is "less than 400 lines of safe, async Rust" for exposing local ports to a remote server — [conduktor/bore (archived fork)](https://github.com/conduktor/bore).
  - frp supports PROXY protocol, as noted under (a).

### Inferences
**Design options (inbound), with tradeoffs:**

1. **A1. aimx-relay as MX, with store-and-forward over an outbound tunnel (recommended default for hosted).** The relay terminates SMTP on 25 with STARTTLS. It runs SPF, DKIM, DMARC and ARC on the raw bytes with the real client IP and envelope, writes the message durably, and only then returns 250. The node keeps a persistent outbound WSS or HTTP/2 connection on 443 to the relay. The relay pushes `{envelope, peer_ip, helo, auth_results, raw}` frames; the node acks after its own durable write, and only then does the relay delete.
   - Pros: works behind CGNAT and on laptops, and queues while the node is offline. The relay can hold mail for days, matching RFC 5321's 4-5 day sender norm, so offline nodes don't lose mail. One relay can serve many nodes. Auth results are computed where the client IP is known.
   - Cons: the relay sees plaintext. Mitigate by encrypting each message to the node's public key right after DATA, so only ciphertext is stored; the relay still sees it in memory. The relay also needs abuse and spam controls on inbound volume.
   - aimx changes: `ingest.rs` must accept relay-supplied peer IP and envelope. Either recompute SPF from the forwarded peer IP (mail-auth can verify SPF given any IP, so the node can still compute verdicts itself), or trust a relay-signed Authentication-Results/ARC. Recomputing on the node from the forwarded IP keeps aimx's trust gate (`should_fire_on_receive`) independent of relay honesty. The node then only has to trust the IP and envelope metadata, which the authenticated tunnel should carry.
2. **A2. TCP pass-through relay (frp/rathole-style with PROXY protocol v2).** The relay forwards raw TCP 25 over the tunnel, and the node's existing aimx SMTP server terminates STARTTLS.
   - Pros: the relay never sees plaintext when senders use STARTTLS, and very little relay code is needed. aimx's existing `smtp/` stack and SPF logic work if `session.rs` learns to parse a PROXY v2 header from the trusted tunnel.
   - Cons: no queueing. If the node is offline the relay must 4xx or refuse, which relies on sender retries (risky with short-retry senders like the Yahoo anecdote, and with laptops offline for days). The node needs a valid TLS cert for the MX hostname (ACME via DNS-01, or HTTP-01 proxied by the relay). Multi-tenant SNI-less SMTP routing is awkward: the relay can't know which node owns the connection before RCPT TO, so pure L4 pass-through works only with a dedicated IP or MX hostname per node, or an L7 SMTP front end that reads RCPT and then splices.
   - Fit: good for the self-hosted single-tenant VPS relay. Poor for the shared hosted relay.
3. **A3. Hybrid.** Pass through when the node is connected; terminate and store (encrypted) when it isn't. Best UX, but the relay needs its own TLS cert and both code paths.
4. **A4. Cloudflare Email Routing to Email Worker, then R2/Queue/Durable Object, then the node pulls over WSS (or Worker `fetch()` to the node via an HTTP Cloudflare Tunnel, which needs no client cloudflared for HTTP).**
   - Pros: zero servers to run, free inbound, ARC-preserved auth.
   - Cons: a 25 MiB cap and 200 rules/addresses per domain. The node loses the client IP and must trust Cloudflare's auth verdicts. Cloudflare sees plaintext, and mail failing both SPF and DKIM may not be forwarded. It also likely requires the user's domain on Cloudflare DNS (unverified; see Gaps). Fine as an option for users already on Cloudflare; not a universal answer.
5. **A5. ESP inbound into a pull queue** (SES to S3 plus SNS/SQS; Postmark/Resend webhook into our queue).
   - Pros: offloads the port-25 edge and spam/virus scanning (SES).
   - Cons: webhook ESPs need a public HTTPS endpoint, which the NAT'd node lacks, so you still need a queue or tunnel. Postmark gives up after about 10.5 h, so a laptop asleep overnight could lose mail unless an intermediate store exists. Per-user AWS setup is heavy friction. SES-to-S3 with node polling is the cleanest BYO-cloud option.
6. **A6. User's own VPS** (WireGuard DNAT or frp/rathole), which is the self-host version of A1/A2. It needs a VPS with port 25 unblocked and a PTR record.

**What does not work as an MX path:** Tailscale Funnel (ports 443/8443/10000 only), ngrok TCP (random port), Cloudflare Tunnel TCP (client cloudflared required), and Cloudflare Spectrum on non-Enterprise plans (and even on Enterprise, a Tunnel origin can't receive PROXY protocol, so SPF would break).

**Who sees plaintext:** SMTP TLS is hop-by-hop. In A1, A3, A4 and A5 the relay operator (us, Cloudflare, AWS, Postmark) sees plaintext. Only A2 avoids it, and only when the sender used STARTTLS. Opportunistic STARTTLS without MTA-STS/DANE is still MITM-able by whoever controls the MX IP.

**Where SPF/DKIM/DMARC is evaluated:** SPF must use the IP of the MTA that connected to the MX. In any relay design that is only visible at the relay, so the relay must either evaluate it or forward it faithfully (PROXY v2 or a tunnel frame field). DKIM and DMARC can be evaluated anywhere on unmodified bytes, so the relay must not alter the message.

### Gaps
- Whether Cloudflare Email Routing requires Cloudflare-hosted DNS (nameservers), and whether catch-all works with Workers, were not confirmed this session.
- Cloudflare Agents SDK email handling (agents receiving and replying to email) was not researched (search budget exhausted).
- SendGrid Inbound Parse details and pricing were not retrieved.
- No open-source project was found that does exactly "SMTP MX relay to persistent WebSocket/HTTP2 tunnel to NAT'd mail node" with store-and-forward. The closest are frp/rathole (L4), WireGuard DNAT recipes, and Cloudflare Email Worker webhook samples.
- Whether the `mail-auth` crate (already used by aimx) supports ARC sealing and verification was not verified this session.

---

## 3. Outbound options without port 25 (smarthost, DKIM location, SPF/DMARC alignment, bounces)

### Takeaway
Without port 25, outbound has to be relayed. Either we run the relay (inheriting all IP-reputation and abuse work), or we hand off to an ESP (SES, Postmark, Mailgun, SMTP2GO, Brevo, MailChannels, Cloudflare Email Service).

For DMARC, the cleanest model is for the aimx node to keep DKIM-signing with the user's own key (`d=` the user's domain). The relay delivers the bytes unmodified and may add its own second signature, as SES does.

The biggest lesson from the shared-relay security incidents: a shared SPF include lets every tenant spoof every other tenant's domain. The relay must bind each account to its verified domains (MailChannels "Domain Lockdown").

### Cited Findings
- **Smarthost submission ports:** Microsoft recommends authenticated relays on 587 and says "Using these email delivery services isn't restricted in Azure, regardless of the subscription type" — [Microsoft Learn](https://learn.microsoft.com/hr-hr/azure/virtual-network/troubleshoot-outbound-smtp-connectivity). Hetzner leaves 587 open — [Discourse Meta](https://meta.discourse.org/t/ports-blocked-hetzner-cloud-server/225146?tl=en). DigitalOcean blocks 587 and 465 as well — [DigitalOcean docs](https://docs.digitalocean.com/support/why-is-smtp-blocked/).
- **SES DMARC/SPF alignment:** for DMARC to pass via SPF, the message must pass SPF on a custom MAIL FROM domain that aligns with From, and DMARC must not set `aspf=s` — [AWS SES docs: DMARC](https://docs.aws.amazon.com/ses/latest/dg/send-email-authentication-dmarc.html).
  - Without a custom MAIL FROM, SES uses an amazonses.com Return-Path, so SPF passes but is unaligned — [autospf guide](https://autospf.com/blog/setting-up-spf-dkim-for-amazon-ses/).
  - Custom MAIL FROM needs an MX to `feedback-smtp.<region>.amazonses.com` and an SPF TXT on the MAIL FROM subdomain (secondary) — [captaindns guide](https://www.captaindns.com/en/blog/amazon-ses-transactional-email-technical-guide).
- **Dual DKIM:** every SES message carries a DKIM signature for amazonses.com, so self-signed messages end up with two DKIM headers. SES does not validate manually added signatures, and a bad one can cause rejection — [AWS SES docs: manual DKIM signing](https://docs.aws.amazon.com/ses/latest/dg/send-email-authentication-dkim-manual.html). The amazonses.com signature is described as being "for feedback loops" (vendor) — [unspam.email](https://unspam.email/deliverability/amazon-ses).
- **Shared-SPF spoofing (MailChannels Domain Lockdown):**
  - DMARC has a weakness: domains whose SPF authorizes a large shared IP space can be spoofed by other customers of that shared service — [MailChannels blog: Domain Lockdown](https://blog.mailchannels.com/bolstering-your-email-security-introducing-domain-lockdown/).
  - Including MailChannels SPF without the `_mailchannels` TXT lockdown record "makes it easier for other MailChannels customers to send email that appears to come from your domain." The TXT format is `v=mc1 auth=... senderid=... cfid=...` — [MailChannels docs: SPF records](https://docs.mailchannels.com/outbound/spf-records.md); [MailChannels help: Domain Lockdown](https://support.mailchannels.com/hc/en-us/articles/16918954360845-Secure-your-domain-name-against-spoofing-with-Domain-Lockdown).
  - A 2023 report said about 2 million domains were exposed to this spoofing — [Solace Global, 2023-09-21](https://www.solaceglobal.com/news/2023/09/21/cyber-security-alert-do-you-use-mailchannels-2m-domains-open-to-phishing-attacks/).
- **Gmail all-sender rules** (from 2024-02-01): SPF or DKIM, valid forward and reverse DNS (PTR) for sending IPs, TLS, spam rate below 0.3% in Postmaster Tools, and RFC 5322 formatting — [Google: Email sender guidelines](https://support.google.com/a/answer/81126?hl=en-GB).
- **Forwarders and ARC:** vendor coverage says Google's guidelines tell forwarders and mailing lists to add ARC headers — [PowerDMARC](https://powerdmarc.com/google-arc-sender-guidelines/); [EasyDMARC](https://easydmarc.com/blog/arc-email-authentication-what-it-is-and-how-it-works/).
- **Self-hosted relay design:** in the split design, the VPS relay delivers outbound while the home server DKIM-signs — [administrator.de](https://administrator.de/tutorial/mailserver-relay-selfhosted-reputation-677373.html). Outbound must leave from the SPF-authorized IP, with HELO matching the PTR — [Lemmy](https://lemmy.brendan.ie/comment/910050).
- **Cloudflare Email Service (outbound):**
  - Private beta 2025-09-25 — [Cloudflare blog: Email Service](https://blog.cloudflare.com/email-service/).
  - Public beta reported 2026-04-16 during Agents Week (secondary) — [captaindns](https://www.captaindns.com/en/blog/cloudflare-email-service); [InfoQ 2025-10](https://www.infoq.com/news/2025/10/cloudflare-email-service).
  - Usage is `env.EMAIL.send()` on Workers Paid — [Cloudflare changelog](https://developers.cloudflare.com/changelog/product/email-service/).
  - Default total message size is 5 MiB, or 25 MiB to verified destinations only — [Cloudflare Email Service limits](https://developers.cloudflare.com/email-service/platform/limits/).

### Inferences
**Design options (outbound):**

1. **B1. Node-signed DKIM plus aimx-relay delivery (recommended).**
   - The node keeps its current DKIM signing (`send_handler.rs` with `Arc<DkimKey>`), so the user's key never leaves the node and DKIM aligns with the From domain. DMARC passes on DKIM alone, so SPF alignment is optional.
   - The relay adds a second signature with `d=relay-domain` (like SES's amazonses.com) for Feedback-ID/FBL attribution, and delivers from IPs with proper FCrDNS.
   - Return-Path uses a relay bounce domain (e.g., `b+<msgid>@bounce.relay.example`, VERP-style) so DSNs come back to the relay. The relay maps them to the node over the tunnel and feeds a per-account suppression list. This SPF passes for the relay domain but is unaligned, which DMARC tolerates as long as DKIM aligns.
   - Optionally offer an aligned bounce subdomain via CNAME/MX (SES custom MAIL FROM pattern) for users who want both SPF and DKIM alignment.
   - The relay must re-verify that the submitted message's DKIM `d=` and From domain belong to the authenticated account (Domain Lockdown equivalent). Otherwise any tenant can send as any other tenant via the shared SPF include.
2. **B2. Relay-signed DKIM** (the user publishes a CNAME to a relay-managed DKIM key, like SES Easy DKIM or AgentMail's returned records). This lowers node complexity and allows key rotation by the operator, but the relay can sign anything for that domain, which is a larger trust surface. It is needed anyway for the zero-DNS hosted-subdomain mode (Q5).
3. **B3. Delegate to an ESP** (SES, Postmark, Mailgun, SMTP2GO, MailChannels, Cloudflare Email Service), either from the node directly via HTTPS API or from our relay. This offloads IP reputation, FBLs and abuse. The costs: account vetting (SES sandbox, SMTP2GO review), per-provider APIs, and ESP ToS that may forbid agent-generated or cold mail. BYO-ESP mode (user pastes SES/Postmark creds into aimx and the node submits over 443) is cheap to build and avoids us being the abuse operator.
4. **B4. Self-hosted single-tenant relay on a VPS** (the same aimx-relay binary): the user's own IP and reputation, with no multi-tenant abuse problem. They must get port 25 unblocked and set PTR.

- **Transport for outbound submission:** use the same authenticated WSS/HTTP2-over-443 tunnel as inbound, not SMTP 587. This avoids DO's 587 block and the Fly-style SMTP timeouts, and reuses one connection and auth model.
- **Message modification:** the relay must not alter body or signed headers (no footers or tracking rewrites), or node DKIM breaks.

### Gaps
- No primary source captured on VERP or bounce-address conventions, or DSN parsing at shared relays (search budget exhausted).
- Postmark's custom Return-Path (CNAME) mechanics were not captured.
- SPF 10-DNS-lookup limit implications of adding a relay include were not sourced.

---

## 4. Operating a shared outbound relay: reputation, abuse controls, mailbox-provider rules, comparables, MailChannels history, legal exposure

### Takeaway
A shared relay is an abuse-operations business, not just code. Gmail, Yahoo and Microsoft now enforce authentication and spam-rate limits:
- Gmail: all-sender rules from Feb 2024, with enforcement ramping since Nov 2025.
- Yahoo: from Feb 2024, with one-click unsubscribe enforced from Jun 2024.
- Microsoft (Outlook consumer): from 5 May 2025.

Comparable providers survive with layered controls: new-account quotas plus manual review (SMTP2GO, SES sandbox), mandatory verified sender domains, per-sender behavioral reputation with automatic shutdown (MailChannels), hard daily caps (ImprovMX), and no free outbound at all (ImprovMX free tier).

MailChannels' free Cloudflare Workers sending ended 2024-08-31. The official stated reason was vague. Its shared-SPF spoofing exposure and the resulting mandatory Domain Lockdown are the documented context.

### Cited Findings

**Mailbox-provider requirements**
- **Gmail:**
  - All senders (since 2024-02-01): SPF or DKIM, PTR, TLS, spam below 0.3%, RFC 5322 — [Google sender guidelines](https://support.google.com/a/answer/81126?hl=en-GB).
  - Bulk senders (5,000+ messages per day to Gmail): SPF and DKIM and DMARC, and one-click unsubscribe for marketing mail only (transactional excluded). Senders above 0.3% are ineligible for mitigation until they stay below 0.3% for 7 consecutive days. Google treats above 0.1% as harmful — [Google support 14229414](https://support.google.com/a/answer/14229414?hl=ense); [Redsift](https://redsift.com/resources/blog/gmails-enforcement-ramps-up-what-bulk-senders-need-to-know).
  - Vendor blogs say Gmail ramped up enforcement on non-compliant traffic from November 2025. Sources disagree on whether failures cause rejection or spam-foldering — [Redsift](https://redsift.com/resources/blog/gmails-enforcement-ramps-up-what-bulk-senders-need-to-know); [GMass](https://www.gmass.co/blog/gmail-bulk-sender-guidelines/); [Suped](https://www.suped.com/knowledge/email-deliverability/technical/how-will-gmail-enforce-new-email-authentication-requirements-and-what-should-senders-do).
- **Google Postmaster Tools v2:** a Compliance Status dashboard (pass or needs-work for SPF, DKIM, DMARC, From alignment, one-click unsubscribe, spam rate) replaced the reputation charts. The date is reported as 2025-09-30 by one vendor, and is disputed — [Blueshift](https://blueshift.com/blog/google-postmaster-tools-v2/). There is an API (`domains.getComplianceStatus`) — [Google Developers](https://developers.google.com/workspace/gmail/postmaster/reference/rest/v2/domains/getComplianceStatus?hl=hi). FBL data requires a Feedback-ID header — [customer.io docs](https://docs.customer.io/messaging/channels/email/deliverability/google-postmaster-tools/); [AWS Messaging blog](https://aws.amazon.com/blogs/messaging-and-targeting/understanding-google-postmaster-tools-spam-complaints-for-amazon-ses-email-senders/).
- **Yahoo:** SPF and DKIM, DMARC at least p=none and aligned, one-click unsubscribe (RFC 8058) honored within 2 days, a 0.3% complaint ceiling, valid forward and reverse DNS, and RFC 5321/5322 compliance. Yahoo reportedly does *not* publish a volume threshold and classifies at the From-domain level. One-click unsubscribe enforcement began June 2024 (secondary) — [Palisade](https://www.palisade.email/learning/yahoo-bulk-sender-requirements.md); [egressif](https://egressif.io/resources/sender-requirements/yahoo).
- **Microsoft (Outlook.com consumer: hotmail, live, outlook):** since 2025-05-05, domains sending more than 5,000 per day must pass SPF, DKIM and DMARC. Non-compliant mail goes to junk, with eventual rejection as `550; 5.7.515` — [Microsoft Postmaster policies](https://substrate.office.com/ip-domain-management-snds/Postmaster/Policies); [Security Boulevard 2025-04](https://securityboulevard.com/2025/04/microsoft-strengthens-email-sender-rules-key-updates-you-shouldnt-miss/).

**How comparables handle abuse**
- **MailChannels Outbound Filtering:**
  - Every sending user, app or script builds its own reputation score. It tracks "dozens of minute-by-minute statistics about each email sender" (volume, content, recipient-validation failures, receiver responses) and applies rate limiting or blocking. Compromised accounts are shut down automatically with a notification — [MailChannels docs: outbound overview](https://docs.mailchannels.com/outbound/overview); [MailChannels help](https://support.mailchannels.com/hc/en-us/articles/207445806-How-does-MailChannels-Outbound-Filtering-work); [MailChannels blog](https://blog.mailchannels.com/detecting-and-blocking-compromised-accounts/).
  - Outbound Filtering for hosts starts at $99.99/month (secondary) — [freetier.co](https://freetier.co/directory/products/mailchannels).
- **SMTP2GO:**
  - All new accounts start with a 1,000-email cap and a review-team assessment; mail beyond the cap queues until activation. Unsolicited mail is banned, and lists must be permission-based and mailed within the last 3 months — [SMTP2GO support: New Account Limits and Review](https://support.smtp2go.com/hc/en-gb/articles/223087427-New-Account-Limits-and-the-Review-Process).
  - From 2025-04-11 every email must come from a verified sender (domain or address) — [SMTP2GO blog](https://www.smtp2go.com/blog/registering-a-verified-sender-is-now-a-required-action/).
  - Free plan: 1,000/month and 200/day, with 25/hour if no domain is verified (secondary) — [hackceleration](https://hackceleration.com/labs/smtp2go-pricing).
- **ImprovMX:** free accounts can receive 500/day and send 0. SMTP is paid-only, with 200/day on Premium and 1,000/day on Pro (tier names conflict across ImprovMX pages). When a domain exceeds its limit, ImprovMX stops processing mail for it until 00:00 UTC. Spam, phishing and mailing-list use bring immediate suspension — [ImprovMX help: limits](https://help.improvmx.com/en/articles/4342296-what-are-the-sending-receiving-limits); [ImprovMX abuse policy](https://improvmx.com/abuse); [ImprovMX guide: forwarding vs SMTP](https://improvmx.com/guides/forwarding-vs-smtp).
- **Amazon SES:** production access review asks how you build lists, handle bounces and complaints, let recipients opt out, and chose your requested quota — [AWS Messaging blog](https://aws.amazon.com/blogs/messaging-and-targeting/how-large-senders-can-move-from-sandbox-to-production-using-amazon-ses/); [oneuptime](https://oneuptime.com/blog/post/2026-02-12-move-amazon-ses-out-of-sandbox/markdown).
- **Cloudflare:**
  - Email Routing refuses to forward unauthenticated mail (SPF or DKIM required since 2025-07-03) — [Cloudflare changelog](https://developers.cloudflare.com/changelog/product/email-routing/).
  - Email Service sending is limited to Workers Paid. Sends to verified destination addresses are free and outside quota. Hard bounces count toward quota — [Cloudflare Email Service pricing](https://developers.cloudflare.com/email-service/platform/pricing/).
- **Forward Email:** no specific information on its outbound abuse controls was found this session.

**MailChannels free Workers sending: what happened**
- MailChannels launched the free Workers send API in early 2022 to learn about developer needs. It announced termination "after careful consideration," first for 2024-06-30, then ended it 2024-08-31. Users were pointed to the MailChannels Email API, whose free plan allows 100 emails per day — [MailChannels blog](https://blog.mailchannels.com/important-update-mailchannels-email-sending-api-for-cloudflare-workers-to-be-terminated/); [MailChannels EOL notice](https://support.mailchannels.com/hc/en-us/articles/26814255454093-End-of-Life-Notice-Cloudflare-Workers).
- Domain Lockdown (`cfid=` for Workers) became mandatory for Workers users before the shutdown — [MailChannels help](https://support.mailchannels.com/hc/en-us/articles/16918954360845-Secure-your-domain-name-against-spoofing-with-Domain-Lockdown).
- The 2023 spoofing exposure: about 2M domains, and a claim that "anyone could register for a low-cost account and spoof customer domains" — [Solace Global 2023](https://www.solaceglobal.com/news/2023/09/21/cyber-security-alert-do-you-use-mailchannels-2m-domains-open-to-phishing-attacks/).
- The MailChannels Email API free plan is 100/day, and until a payment method is added, sending is restricted to verified users' addresses — [MailChannels docs: pricing](https://docs.mailchannels.com/email-api/billing/pricing).

**Reputation and warm-up**
- On shared IPs reputation is spread across senders, so "your deliverability can be impacted by the actions of other senders on the same IP." Dedicated IPs need gradual warm-up (vendor guidance) — [Suped](https://www.suped.com/email-deliverability/answers/is-ip-warming-necessary-when-migrating-esps-to-shared-ips-and-what-are-the-best-practices); [SendGrid blog: IP warm-up](https://sendgrid.com/blog/ip-warm-up-for-email-deliverability).
- UCEPROTECT Level 3 lists entire ASNs. Experts say most major mailbox providers don't use L3 as a primary block source, though smaller receivers may. Only the hosting provider can resolve it, and paid removal doesn't help — [Suped](https://www.suped.com/learn/blocklists/is-uceprotect-l3-a-blacklist-worth-worrying-about-for-email-deliverability); [MailReach](https://www.mailreach.co/blog/uceprotectl3-blacklist).
- Postal (open-source, self-hosted Mailgun alternative) supports multi-IP pools assigned per organization, server or domain, for warm-up and reputation separation — [openalternative](https://openalternative.co/postal); [learnwithhasan guide](https://learnwithhasan.com/guide/self-host-postal-smtp/).

**Legal exposure (US)**
- Under CAN-SPAM, "initiate" excludes actions that constitute "routine conveyance": automated transmission, routing, relaying, handling or storage of mail where another person picked the recipients. Liability generally attaches to the initiator and to the "sender" whose product is promoted — [15 U.S.C. 7702 (govinfo)](https://www.govinfo.gov/content/pkg/USCODE-2023-title15/html/USCODE-2023-title15-chap103.htm); [Justia 2024 U.S. Code](https://law.justia.com/codes/us/title-15/chapter-103/sec-7702/).

### Inferences
**Minimum abuse controls for a hosted aimx relay, modeled on the comparables:**
1. Verified-domain binding per account: a DNS TXT `_aimx.<domain>` record naming the account (Domain Lockdown). The relay rejects any message whose From or DKIM `d=` isn't bound.
2. New-account quotas (e.g., low daily caps) with automatic step-ups based on bounce and complaint data, plus manual review above a tier (SMTP2GO model).
3. Per-node and per-mailbox behavioral rate limiting and auto-suspend on spikes in bounce, complaint or unknown-recipient rates (MailChannels model).
4. Hard daily caps that stop sending rather than queue (ImprovMX model).
5. Feedback-ID headers and FBL enrollment (Gmail Postmaster v2 via Feedback-ID; Yahoo/Microsoft programs; see Gaps).
6. Optional content scanning for phishing and malware (e.g., rspamd or SpamAssassin on outbound).
7. Possibly no free outbound to arbitrary recipients at all: free tier inbound-only or capped at very low volume (ImprovMX and Cloudflare-verified-destinations precedent).

**Agent-specific risk:** AI agents can generate high volumes of novel content quickly, and a prompt-injected agent could send spam or phishing. aimx's hook trust model helps inbound, but outbound needs relay-side rate limits regardless of node honesty. The relay must treat every node as potentially compromised.

**Why MailChannels' free tier probably died:** the documented facts are a free shared relay, a shared SPF include enabling cross-tenant spoofing (2023), mandatory lockdown, and then shutdown. That fits a pattern where free, unvetted, shared sending is not sustainable. The official statement does not say this explicitly; this is an inference.

**Bulk-sender rules mostly won't apply to agent-to-human transactional mail.** Agent mail is usually low-volume per domain, and one-click unsubscribe applies only to marketing. But Gmail's all-sender rules (PTR, TLS, SPF/DKIM, spam below 0.3%) apply from message one, and a shared relay aggregates many tenants onto the same IPs. So the relay's IP reputation is collectively at risk even if each tenant is tiny.

**Legal:** CAN-SPAM's routine-conveyance carve-out suggests a pure relay that doesn't pick recipients or promote its own products isn't the "initiator." But AUP enforcement, abuse-desk responsiveness and ISP blocklisting are the practical exposure, not statute.

### Gaps
- MailChannels' exact business reason for ending free Workers sending was not stated in sources found. HN discussion threads were not reached.
- Microsoft JMRP/SNDS and Yahoo CFL enrollment details and requirements were not captured (search budget exhausted).
- No M3AAWG primary documents were retrieved (only vendor warm-up guidance).
- Non-US legal exposure (EU ePrivacy/GDPR as processor of message content, UK PECR, Canada CASL) was not researched.
- Forward Email's outbound abuse model was not found.
- No sourced data on typical warm-up schedules or volumes for a new relay IP.

---

## 5. Domains: hosted subdomain addresses vs custom domains (and how AgentMail and others do it)

### Takeaway
Agent-email competitors remove DNS friction by giving every user addresses on a provider-owned domain by default. AgentMail uses `@agentmail.to`, free tier included. Resend uses `*.resend.app` for receiving. Custom domains are a paid upgrade, provisioned via an API that returns the SPF, DKIM, DMARC and MX records to publish. For aimx, a hosted `<agent>@<user>.<aimx-domain>` mode is the only way to deliver a zero-DNS, zero-port-25 experience. It concentrates reputation risk on the provider domain, which argues for a separate registrable domain, not the aimx brand domain.

### Cited Findings
- AgentMail addresses default to "inboxes on the agentmail.to domain" — [AgentMail blog: custom domains for thousands of agent inboxes](https://www.agentmail.to/blog/custom-domains-for-thousands-of-agent-inboxes). The free tier has 3 inboxes and 3,000 emails per month, with no custom domains — [nocode.mba](https://www.nocode.mba/articles/agentmail-ai-agent-email).
- AgentMail pricing (official, as of 2026-09-17 per the search snippet): Developer $20/month with 10 custom domains; Startup $200/month with 150; Enterprise custom. Pay-as-you-go adds one custom domain for $2/month — [AgentMail pricing](https://www.agentmail.to/pricing.md); [AgentMail blog](https://www.agentmail.to/blog/custom-domains-for-thousands-of-agent-inboxes).
- **AgentMail custom-domain workflow:**
  - `domains.create` returns SPF, DKIM, DMARC and MX records. Once the domain is VERIFIED, you bulk-create inboxes with `inboxes.create(username, domain)` — [AgentMail docs: custom domains](https://docs.agentmail.to/custom-domains); [AgentMail KB: custom domain setup](https://docs.agentmail.to/knowledge-base/custom-domain-setup).
  - Use `subdomains_enabled` for address space under one parent. Use separate domains when cold outreach and receipts must not share DKIM reputation. Use "Pods" per customer for multi-tenant products — [AgentMail blog](https://www.agentmail.to/blog/custom-domains-for-thousands-of-agent-inboxes).
  - DKIM record type differs between doc pages (TXT vs CNAME) — [AgentMail docs](https://agentmail.to/docs/managing-domains.md).
- Resend lets you receive on a Resend-managed `*.resend.app` domain or your own MX — [Resend blog: Inbound Emails](https://resend.com/blog/inbound-emails).
- Cloudflare Email Routing supports subdomains (announced alongside its ARC support) — [Cloudflare blog: Email Routing subdomains](https://blog.cloudflare.com/email-routing-subdomains/).
- Yahoo classifies sender reputation "at the authenticated domain / From header domain level" (secondary) — [Palisade](https://www.palisade.email/learning/yahoo-bulk-sender-requirements.md).

### Inferences
**Three domain modes for aimx:**
1. **Hosted subdomain** (`agent@alice.<aimx-mail-domain>`). Wildcard MX points at the aimx relay, and DKIM is relay-signed (or node-signed with a key the relay publishes in our DNS). No user DNS and no port 25 needed: the best onboarding, matching AgentMail and Resend defaults. Risk: all tenants share the parent's organizational-domain reputation. Mitigations:
   - Use a dedicated registrable domain separate from the aimx project or brand domain.
   - Apply strict abuse controls (Q4).
   - Consider splitting free vs paid onto different domains (AgentMail's own advice to separate reputations).
2. **Custom domain via relay.** The user sets MX to the relay, adds an SPF include, publishes a node DKIM public key (or a CNAME to a relay key), and adds the `_aimx` lockdown TXT and DMARC. The existing `aimx setup` DNS section already prints records and could gain a "relay mode" variant.
3. **Custom domain, direct** (today's aimx: port 25 open, VPS).

- Inbound hosted subdomains also simplify relay routing. The relay maps `*.alice.<domain>` to Alice's node tunnel by RCPT domain, with no per-domain MX verification.

### Gaps
- How domain reputation propagates between subdomains and the organizational domain at Gmail and Microsoft was not sourced. Neither was whether listing the hosted parent on the Public Suffix List would isolate subdomain reputation (this would need research before choosing).
- AgentMail's underlying sending infrastructure (own MTAs vs SES) was not determined. The "AgentMail vs SES" page discussed migration only.
- Other agent-mail products' domain models were not surveyed beyond AgentMail and Resend (search budget).

---

## 6. Cost model: per-1k-email costs (ESPs vs own relay)

### Takeaway
At small volume, raw ESP sending is cheap:
- SES: about $0.10-0.16 per 1k outbound and about $0.10 per 1k inbound plus chunk fees.
- Cloudflare Email Service: $0.35 per 1k after 3k/month included on Workers Paid.
- Mailgun: about $0.70-1.50 per 1k effective on base plans.
- Postmark: about $1.50-1.80 per 1k at the 10k tier.

A self-run relay's marginal cost per email is near zero. Its real costs are clean IPs (a Mailgun dedicated IP is $60/month, a useful benchmark), abuse-desk staff time and reputation risk. Prices below are mostly from 2026 aggregator pages, so confirm on official pages before budgeting.

### Cited Findings
- **Amazon SES:** à la carte outbound $0.10 per 1k. Bundled plans launched 2026-07-21, and new accounts start on Essentials at $0.16 per 1k with a deliverability dashboard; you can switch back to à la carte — [dev.to (2026)](https://dev.to/mr_manushukla/amazon-ses-pricing-plans-in-2026-essentials-vs-pro-vs-enterprise-and-when-a-la-carte-still-wins-1ohg); [ecorpit](https://ecorpit.com/amazon-ses-pricing-plans-essentials-pro-enterprise-decision-2026/).
  - Inbound is $0.10 per 1k. The chunk fee is $0.09 or $0.10 per 1k 256 KB chunks (sources conflict). Data transfer is about $0.12/GB (secondary) — [usecarly](https://www.usecarly.com/blog/amazon-ses-pricing/); [toolradar](https://toolradar.com/tools/amazon-ses/pricing).
- **Cloudflare Email Service:** Workers Paid includes 3,000 outbound per month per account, then $0.35 per 1k. Sends to verified destination addresses are free. Workers Free has no outbound — [Cloudflare Email Service pricing](https://developers.cloudflare.com/email-service/platform/pricing/). Email Routing (inbound) is free; the 200 rules/addresses limit applies — [Cloudflare limits](https://developers.cloudflare.com/email-routing/limits).
- **Postmark** (secondary, 2026): Free 100/month; Basic $15 for 10k ($1.80/1k overage); Pro $16.50 for 10k ($1.30/1k) with inbound; Platform $18 for 10k ($1.20/1k) with inbound — [automationatlas](https://automationatlas.io/answers/postmark-pricing-explained-2026/); [klymentiev](https://klymentiev.com/blog/postmark-pricing).
- **Mailgun** (secondary, 2026): Free 100/day; Basic $15 for 10k ($1.80/1k overage); Foundation $35 for 50k; Scale $90 for 100k. A dedicated IP is $60/month. Some sources list Foundation at $75 for 100k (conflict) — [automationatlas](https://automationatlas.io/answers/mailgun-pricing-explained-2026/); [usecarly](https://www.usecarly.com/blog/mailgun-pricing/).
- **SMTP2GO:** free 1k/month (200/day). Starter $10 or $15 for 10k (sources conflict). Professional $75 for 100k, with dedicated IP from that tier — [hackceleration](https://hackceleration.com/labs/smtp2go-pricing); [Capterra](https://www.capterra.com/p/166646/SMTP2GO/pricing/).
- **Brevo:** free 300/day; Starter $9 for 5k; $18 for 20k (secondary) — [toolquestor](https://toolquestor.com/vs/smtp2go-vs-brevo); [sequenzy](https://www.sequenzy.com/alternatives/smtp2go-alternatives).
- **MailChannels Email API:** free 100/day; paid "from $10/month" (secondary, unverified) — [MailChannels docs](https://docs.mailchannels.com/email-api/billing/pricing); [freetier.co](https://freetier.co/directory/products/mailchannels).
- **AgentMail** (an agent-email comparable, for positioning): Free with 3 inboxes and 3k emails per month; Developer $20/month; Startup $200/month — [AgentMail pricing](https://www.agentmail.to/pricing.md); [nocode.mba](https://www.nocode.mba/articles/agentmail-ai-agent-email).

### Inferences
**Effective cost per 1k outbound at about 10k/month:**

| Provider | Effective $/1k | Basis |
|---|---|---|
| SES | ~$0.10-0.16 | Plus chunk/transfer, and a dedicated IP extra if wanted |
| Cloudflare Email Service | ~$0.35 | Beyond 3k; plus the Workers Paid subscription |
| Mailgun Basic | ~$1.50 | $15 / 10k |
| Postmark Basic | ~$1.50 | $15 / 10k |
| SMTP2GO Starter | ~$1.00-1.50 | |

At 100k/month: Mailgun Scale is about $0.90/1k and SMTP2GO Professional about $0.75/1k.

**Own relay:** the compute cost is a small VPS (price not sourced this session; see Gaps) plus IPv4 addresses with PTR. The dominant costs are operational: an abuse desk, monitoring Postmaster/SNDS, delisting work, and warm-up. If aimx's hosted user base sends little volume, piggybacking the relay's outbound leg on SES (relay delivers to SES over 587 or API with BYODKIM/custom MAIL FROM) is far cheaper than the reputation work of clean IPs. The relay would then mainly be an inbound MX plus a tunnel broker.

**Inbound:** Cloudflare Email Routing is $0. SES is about $0.10/1k plus chunks. Our own relay's inbound cost is mostly storage and bandwidth. Inbound is cheap in every design, and outbound reputation is the expensive part.

### Gaps
- Current VPS prices for port-25-capable providers (Hetzner, OVH, Vultr, Linode) were not retrieved. Prior knowledge suggests single-digit USD per month for the smallest instances, but this was not verified this session.
- The Cloudflare Workers Paid base subscription price was not captured.
- Official pricing pages (AWS, Postmark, Mailgun, SMTP2GO, Brevo) could not be fetched. Figures are from 2026 aggregators and some conflict.
- SendGrid pricing and Inbound Parse were not retrieved.

---

## 7. Existing reference implementations (open source) to learn from

### Takeaway
No turnkey open-source "SMTP MX relay to persistent tunnel to NAT'd mail node with store-and-forward" was found. The building blocks are mature:
- L4 reverse tunnels with PROXY protocol: frp, rathole, bore.
- WireGuard-plus-VPS DNAT recipes used with Mailcow, docker-mailserver and Postfix.
- Full MTAs with queues: Stalwart (Rust) and Postal (with IP pools, bounces and webhooks).
- Cloudflare Email Worker webhook samples.
- PROXY-protocol-aware SMTP server libraries.

aimx already depends on `mail-auth`, the Stalwart-family auth crate, for SPF, DKIM and DMARC.

### Cited Findings
- **frp:** PROXY protocol v1/v2 per proxy (`transport.proxyProtocolVersion`) for real client IP on any TCP service — [gofrp.org: Get Real User IP](https://gofrp.org/en/docs/features/common/realip/).
- **rathole:** Rust frp alternative, token-authenticated, TOML config, binary as small as ~500 KiB, claims higher connection capacity and lower memory than frp — [rapiz1/rathole](https://github.com/rapiz1/rathole); [rathole-org/rathole](https://github.com/rathole-org/rathole).
- **bore:** under 400 lines of async Rust, `bore local <port> --to <server>` — [conduktor/bore (archived fork)](https://github.com/conduktor/bore); [tctien342/bore (fork)](https://github.com/tctien342/bore). Upstream status was not verified.
- **Mailcow + WireGuard + FRP VPS tunneling guide** — [GitHub gist](https://gist.github.com/sussycatgirl/4bcd6508dedd21ea21ec494499c92618). A docker-mailserver discussion covers relaying inbound and outbound via another host — [docker-mailserver discussion #3965](https://github.com/orgs/docker-mailserver/discussions/3965).
- **Stalwart:** an all-in-one Rust mail server (AGPL-3.0) with MTA-STS, TLSRPT, DANE and DMARC, virtual queues, routing rules and throttling. Config format changed in v0.16 — [stalwartlabs/smtp-server](https://github.com/stalwartlabs/smtp-server); [Stalwart blog](https://stalw.art/blog/tags/mta-sts/); [NixOS wiki](https://wiki.nixos.org/wiki/Stalwart/Legacy_setup).
- **Postal:** a self-hosted Mailgun/Postmark-like delivery platform with SMTP and HTTP API, bounce handling, delivery webhooks and multi-IP pools. It is not a mailbox server. Reported at about 16.2k stars and MIT license (secondary) — [openalternative: Postal](https://openalternative.co/postal); [learnwithhasan](https://learnwithhasan.com/guide/self-host-postal-smtp/).
- **Cloudflare Email Worker examples:** [nhdms/cloudflare-email-webhook-worker](https://github.com/nhdms/cloudflare-email-webhook-worker); [kill-the-news](https://git.unsupervised.ca/GitHub/kill-the-news/src/commit/59cbbd04286895441c26851b103c6fe2cfb92ba1); [tnix100/CF-Email-Worker](https://github.com/tnix100/CF-Email-Worker).
- **PROXY-protocol SMTP servers:** [aiosmtpd](https://aiosmtpd.aio-libs.org/en/latest/proxyprotocol.html); [nodemailer smtp-server](https://mintlify.com/nodemailer/smtp-server/guides/proxy-protocol).
- **A curated list of VPS providers' port-25/PTR policies** (maintained by Forward Email) — [forwardemail/awesome-mail-server-providers](https://github.com/forwardemail/awesome-mail-server-providers).

### Inferences
**Suggested build** (Rust, to match aimx): a new `aimx relay` subcommand or a separate crate, as the verifier already is.
- Inbound SMTP listener: reuse aimx's `smtp/` session state machine and `mail-auth`.
- Durable spool: per-tenant, encrypted to the node's public key.
- Tunnel endpoint: WSS or HTTP/2 on 443 with mutual auth (node key pair registered at setup). Frames carry envelope, peer IP, HELO, TLS info, the raw message, and outbound submissions.
- Outbound queue with retries, DSN parsing and suppression. Stalwart's queue design and Postal's IP pools and bounce handling are references.
- One binary serves both modes: hosted multi-tenant (with Q4 abuse controls) and self-hosted single-tenant on a user's VPS.

**rathole/frp (L4) for the self-host mode:** these could be bundled or recommended immediately as a stopgap (port 25 on the VPS to the node, PROXY v2), with aimx `session.rs` learning to parse PROXY headers from a configured trusted source. Cheap to ship, but no offline queueing.

**The existing verifier** (`services/verifier`, axum + SMTP on 25, Docker, host networking) is already a hosted port-25 service. It's the natural seed for the hosted relay's ops footprint (same deployment model), though the relay needs far more: storage, queues and abuse tooling.

### Gaps
- No search results showed an open-source project that implements an SMTP-over-WebSocket/HTTP2 relay with store-and-forward specifically.
- Stalwart smarthost/relay configuration specifics and Postal's current maintenance status were not verified.
- `mail-auth` ARC support and Rust PROXY-protocol crates were not verified this session.
