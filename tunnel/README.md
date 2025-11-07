# Tunnel

Secure remote access system with HTTP/HTTPS reverse proxy and automatic TLS certificate provisioning.

## Overview

Tunnel has two main components:

1. Reverse proxy with persistent tunnels and automatic certificates - Maintains persistent QUIC connections to registered clients, accepts incoming HTTPS traffic, provisions Let's Encrypt certificates via DNS-01 challenges, and routes requests to the appropriate client based on domain
2. OAuth2 authentication and user management - GitHub OAuth for users, device flow for client registration, domain assignment and authorization

## Architecture

The server runs a centralized hub with several subsystems:
- QUIC listener for persistent client tunnel connections
- HTTPS listener that routes public traffic to clients via SNI
- HTTP listener for web UI and REST API
- DNS server for public queries and ACME challenges
- Certificate maintainer for automatic renewal
- SQLite database for users, domains, tokens, and DNS records

The client connects to a tunnel server and maintains a persistent QUIC connection. It accepts bidirectional streams from the server carrying incoming HTTPS requests, terminates TLS, and forwards traffic to a local handler (typically an HTTP server). The client reconnects automatically with exponential backoff on connection failure.

## Authentication flow

New clients go through interactive setup that uses device OAuth flow. The client requests a device code from the server and displays a user code. The user confirms the code in their browser (authenticating via GitHub OAuth). After approval, the client receives an auth token tied to a selected domain. The token is used to authenticate the persistent QUIC connection.

## Traffic flow

External HTTPS requests arrive at the server's HTTPS listener. The server parses SNI from the TLS ClientHello to identify the target domain, looks up the persistent QUIC connection for that domain's client, opens a bidirectional stream, and forwards the request. The client receives the stream, terminates TLS, handles the request, and sends the response back.

## Certificate provisioning

Certificates are provisioned automatically via Let's Encrypt ACME with DNS-01 validation. The certificate maintainer requests certs from ACME, writes challenge TXT records to the local DNS server, waits for ACME validation, and downloads the certificate. Certificates are hot-swapped without restart. A background loop monitors renewal windows and renews before expiry.

## Local testing

Local testing infrastructure exists using a Pebble ACME server (test CA) and local DNS server. This allows development without production DNS or certificates.

Note: Local testing support is work in progress and needs improvement for DNS resolution, certificate trust, and documentation.
