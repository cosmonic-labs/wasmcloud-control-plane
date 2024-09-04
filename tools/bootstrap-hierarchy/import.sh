#!/bin/bash
set -euo pipefail

# Configure nsc to use local paths
export NKEYS_PATH=keys
nsc env -s store

nsc add operator -u work/operator.jwt
nsc import keys --dir work/keys

find work/accounts -type file -print0 | xargs -n 1 -I {} -0 nsc import account --file "{}" --overwrite
find work/users -type file -print0 | xargs -n 1 -I {} -0 nsc import user --file "{}" --overwrite

# Generate creds for auth callout
mkdir -p callout
nsc generate creds -a 'WASMCLOUD SYSTEM AUTH' -n 'WASMCLOUD SYSTEM AUTH-writer' > callout/auth-writer.creds
nsc generate creds -a 'WASMCLOUD SYSTEM AUTH' -n 'WASMCLOUD SYSTEM AUTH-registration' > callout/auth.creds
nsc generate creds -a 'WASMCLOUD USER AUTH' -n 'WASMCLOUD USER AUTH-writer' > callout/user-auth-writer.creds
nsc generate creds -a 'WASMCLOUD USER AUTH' -n 'WASMCLOUD USER AUTH-registration' > callout/user-auth.creds
account_id=$(nsc describe account 'WASMCLOUD SYSTEM' -J | jq -r .sub)
xkey=$(nsc describe account 'WASMCLOUD SYSTEM AUTH' -J | jq -r .nats.authorization.xkey)
signing_key=$(nsc describe account 'WASMCLOUD SYSTEM' -J | jq -r '.nats.signing_keys | map(objects)[0].key')

cp "keys/keys/${xkey:0:1}/${xkey:1:2}/${xkey}.nk" callout/server.xkey
cp "keys/keys/${signing_key:0:1}/${signing_key:1:2}/${signing_key}.nk" callout/signing.nk
echo "$account_id" > callout/account_id
