# Bootstrap wasmCloud NATS Hierarchy

This tool provisions a [NATS operator
hierarchy](https://docs.nats.io/running-a-nats-service/configuration/securing_nats/auth_intro/jwt)
for a wasmCloud cluster.

## Prerequisites

* [nsc](https://github.com/nats-io/nsc)

## Usage

```bash
go build
./bootstrap-nats-hierarchy

# Import the keys and JWTs into nsc
./import.sh

# Start the NATS server
# You can use the provided resolver.conf file directly or include it as a
# separate file in a custom configuration
nats-server -c work/resolver.conf

# Push the JWTs to the NATS server
nsc push -a
```

## Accounts
| Name                  | Description                                                                            |
| ----                  | ----                                                                                   |
| SYS                   | Needed for administrative actions in NATS                                              |
| WASMCLOUD SYSTEM      | This account is what all wasmCloud hosts, Wadm and the control plane APIs connect to.  |
| WASMCLOUD SYSTEM AUTH | This account managed auth callout for wasmCloud hosts and other control-plane services |
| WASMCLOUD USER AUTH   | This account managed auth callout for users interacting with wasmCloud using wash      |
