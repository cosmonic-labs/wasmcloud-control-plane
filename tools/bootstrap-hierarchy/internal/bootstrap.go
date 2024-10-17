package internal

import (
	"errors"
	"fmt"
	"log/slog"
	"os"

	"github.com/nats-io/jwt/v2"
	"github.com/nats-io/nkeys"
)

func Bootstrap(outDir string) error {
	opKey, err := genOperatorKey()
	if err != nil {
		return err
	}
	opPubKey, err := opKey.PublicKey()
	if err != nil {
		return err
	}
	opSeed, err := opKey.Seed()
	if err != nil {
		return err
	}
	opSigningKey, err := genOperatorKey()
	if err != nil {
		return err
	}
	opSigningPubKey, err := opSigningKey.PublicKey()
	if err != nil {
		return err
	}
	opSigningSeed, err := opSigningKey.Seed()
	if err != nil {
		return err
	}

	opClaims := jwt.NewOperatorClaims(opPubKey)
	opClaims.StrictSigningKeyUsage = true
	opClaims.SigningKeys.Add(opSigningPubKey)
	opClaims.Name = "wasmCloud Operator"
	// TODO do we need this?
	opClaims.AccountServerURL = "nats://localhost:4222"
	opClaims.SystemAccount = "SYS"
	enc, err := opClaims.Encode(opKey)
	if err != nil {
		return err
	}

	operator := &Operator{
		Claims:  *opClaims,
		Key:     string(opSeed),
		Encoded: enc,
		SigningKeys: []string{
			string(opSigningSeed),
		},
	}

	os.MkdirAll(outDir, 0755)
	userDir := outDir + "/users"
	accDir := outDir + "/accounts"
	keysDir := outDir + "/keys"
	os.MkdirAll(userDir, 0755)
	os.MkdirAll(accDir, 0755)
	os.MkdirAll(keysDir, 0755)

	sysAcct, err := createSystemAccount(opSigningKey)
	if err != nil {
		return err
	}
	//fmt.Println(sysAcct)

	if err := sysAcct.writeToDir(outDir); err != nil {
		return err
	}

	// Reference the system account key now that we have it and resign the claims
	operator.Claims.SystemAccount = sysAcct.Claims.Subject
	opEnc, err := operator.Claims.Encode(opKey)
	if err != nil {
		return err
	}
	operator.Encoded = opEnc

	if err := operator.writeToDir(outDir); err != nil {
		return err
	}

	cfg := NewNatsResolverConfigBuilder(false)
	cfg.Add([]byte(operator.Encoded))
	cfg.SetSystemAccount(sysAcct.Claims.Subject)
	cfg.Add([]byte(sysAcct.Encoded))
	data, err := cfg.Generate()
	if err != nil {
		return err
	}
	if err := os.WriteFile(outDir+"/resolver.conf", data, 0644); err != nil {
		return err
	}

	wasmCloudSysAcct, err := createWasmCloudSystemAccount(opSigningKey)
	if err != nil {
		return err
	}
	if err := wasmCloudSysAcct.writeToDir(outDir); err != nil {
		return err
	}

	wasmcloudSystemKey, err := nkeys.FromSeed([]byte(wasmCloudSysAcct.SigningKeys[0]))
	if err != nil {
		return err
	}

	latticeAcct, err := createLatticeAccount(opSigningKey, wasmCloudSysAcct.Claims.Subject, wasmcloudSystemKey)
	if err != nil {
		return err
	}
	if err := latticeAcct.writeToDir(outDir); err != nil {
		return err
	}

	var kp *nkeys.KeyPair
	for _, v := range wasmCloudSysAcct.Claims.SigningKeys {
		if v, ok := v.(*jwt.UserScope); ok {
			if v.Role == "host" {
				for _, k := range wasmCloudSysAcct.SigningKeys {
					keyPair, err := nkeys.FromSeed([]byte(k))
					if err != nil {
						return err
					}
					kp = &keyPair
				}
			}
		}
	}
	if kp == nil {
		return errors.New("no host user found")
	}

	hUser, err := hostUser(*kp, wasmCloudSysAcct.Claims.Subject)
	if err != nil {
		return err
	}
	if err := hUser.writeToDir(outDir); err != nil {
		return err
	}

	// TODO this should probably be locked down to just the user auth account
	auth, err := authCallout(opSigningKey, "*", "WASMCLOUD USER AUTH")
	if err != nil {
		return err
	}

	if err := auth.Account.writeToDir(outDir); err != nil {
		return err
	}
	if err := auth.AuthUser.writeToDir(outDir); err != nil {
		return err
	}
	if err := auth.RegistrationUser.writeToDir(outDir); err != nil {
		return err
	}
	if err := os.WriteFile(keysDir+"/AUTH-xkey.nk", []byte(auth.XKey), 0644); err != nil {
		return err
	}

	hostCallout, err := authCallout(opSigningKey, wasmCloudSysAcct.Claims.Subject, "WASMCLOUD SYSTEM AUTH")
	if err != nil {
		return err
	}

	if err := hostCallout.Account.writeToDir(outDir); err != nil {
		return err
	}
	if err := hostCallout.AuthUser.writeToDir(outDir); err != nil {
		return err
	}
	if err := hostCallout.RegistrationUser.writeToDir(outDir); err != nil {
		return err
	}
	if err := os.WriteFile(keysDir+"/hostCallout-xkey.nk", []byte(hostCallout.XKey), 0644); err != nil {
		return err
	}

	return nil
}

func genOperatorKey() (nkeys.KeyPair, error) {
	opKey, err := nkeys.CreateOperator()
	if err != nil {
		return nil, err
	}
	return opKey, nil
}

func hostUser(accountKey nkeys.KeyPair, accountPublic string) (*User, error) {
	uk, err := nkeys.CreateUser()
	if err != nil {
		return nil, err
	}

	pub, err := uk.PublicKey()
	if err != nil {
		return nil, err
	}

	uSeed, err := uk.Seed()
	if err != nil {
		return nil, err
	}

	user := jwt.NewUserClaims(pub)
	user.Name = "wasmCloud Host"
	user.IssuerAccount = accountPublic
	user.SetScoped(true)
	enc, err := user.Encode(accountKey)
	if err != nil {
		return nil, err
	}

	u := &User{
		Encoded: enc,
		Key:     string(uSeed),
		Claims:  *user,
	}
	return u, nil
}

func createWasmCloudSystemAccount(opKey nkeys.KeyPair) (*Account, error) {
	accKey, err := nkeys.CreateAccount()
	if err != nil {
		return nil, err
	}
	accPubKey, err := accKey.PublicKey()
	if err != nil {
		return nil, err
	}
	accSeed, err := accKey.Seed()
	if err != nil {
		return nil, err
	}
	accSigningKey, err := nkeys.CreateAccount()
	if err != nil {
		return nil, err
	}
	accSigningPubKey, err := accSigningKey.PublicKey()
	if err != nil {
		return nil, err
	}
	accSigningSeed, err := accSigningKey.Seed()
	if err != nil {
		return nil, err
	}

	sys := jwt.NewAccountClaims(accPubKey)
	sys.Name = "WASMCLOUD SYSTEM"
	sys.SigningKeys.Add(accSigningPubKey)
	sys.Limits.DisallowBearer = true
	sys.Limits.JetStreamLimits.Consumer = -1
	sys.Limits.JetStreamLimits.DiskStorage = -1
	sys.Limits.JetStreamLimits.MemoryStorage = -1
	sys.Limits.JetStreamLimits.Streams = -1
	sys.Limits.JetStreamLimits.MaxAckPending = -1
	sys.Limits.JetStreamLimits.DiskMaxStreamBytes = -1
	sys.Limits.JetStreamLimits.MemoryMaxStreamBytes = -1
	sys.Mappings = map[jwt.Subject][]jwt.WeightedMapping{
		//"wadm.api.*.*.>": {
		//	{
		//		Subject: "wadm.api.{{wildcard(2)}}.>",
		//	},
		//},
		"*.wasmbus.cfg.*.>": {
			{
				Subject: "wasmbus.cfg.{{wildcard(2)}}.>",
			},
		},
		"*.wasmbus.ctl.*.*.>": {
			{
				Subject: "wasmbus.ctl.{{wildcard(2)}}.{{wildcard(3)}}.>",
			},
		},
		"*.wasmbus.evt.*.>": {
			{
				Subject: "wasmbus.evt.{{wildcard(2)}}.>",
			},
		},
		"*.wasmcloud.secrets.>": {
			{
				Subject: "wasmcloud.secrets.>",
			},
		},
	}

	exports := []*jwt.Export{
		{
			Name:                 "wasmCloud config",
			Subject:              "*.wasmbus.cfg.>",
			Type:                 jwt.Service,
			TokenReq:             true,
			AccountTokenPosition: 1,
			Advertise:            false,
		},
		{
			Name:                 "wasmCloud events",
			Subject:              "*.wasmbus.evt.>",
			Type:                 jwt.Stream,
			TokenReq:             true,
			AccountTokenPosition: 1,
			Advertise:            false,
		},
		{
			Name:                 "wasmCloud control commands",
			Subject:              "*.wasmbus.ctl.*.>",
			Type:                 jwt.Service,
			ResponseType:         jwt.ResponseTypeStream,
			AccountTokenPosition: 1,
			TokenReq:             true,
			Advertise:            false,
		},
		{
			Name:                 "wadm",
			Subject:              "wadm.api.*.*.>",
			Type:                 jwt.Service,
			TokenReq:             true,
			AccountTokenPosition: 3,
			Advertise:            false,
			Info: jwt.Info{
				Description: "wadm API",
				InfoURL:     "https://wasmcloud.com/docs/ecosystem/wadm/api",
			},
		},
		{
			Name:                 "wasmCloud Secrets",
			Subject:              "*.wasmcloud.secrets.>",
			Type:                 jwt.Service,
			AccountTokenPosition: 1,
			TokenReq:             true,
			Advertise:            false,
		},
		{
			Name:    "wasmCloud Lattice List",
			Subject: "wasmcloud.lattice.list",
			Type:    jwt.Service,
		},
		{
			Name:         "wasmCloud Lattice Watch",
			Subject:      "wasmcloud.lattice.watch",
			Type:         jwt.Service,
			ResponseType: jwt.ResponseTypeStream,
		},
	}
	sys.Exports.Add(exports...)

	scopedKey, err := nkeys.CreateAccount()
	if err != nil {
		return nil, err
	}
	scopedPubKey, err := scopedKey.PublicKey()
	if err != nil {
		return nil, err
	}
	scopedSeed, err := scopedKey.Seed()
	if err != nil {
		return nil, err
	}

	scoped := jwt.NewUserScope()
	scoped.Key = scopedPubKey
	scoped.Role = "host"
	scoped.Template = jwt.UserPermissionLimits{
		Permissions: jwt.Permissions{
			Pub: jwt.Permission{
				Allow: jwt.StringList{
					">",
				},
				Deny: jwt.StringList{
					"*.*.wrpc.>",
				},
			},
			Sub: jwt.Permission{
				Allow: jwt.StringList{
					">",
				},
				Deny: jwt.StringList{
					"*.*.wrpc.>",
				},
			},
			Resp: &jwt.ResponsePermission{
				MaxMsgs: -1,
			},
		},
	}
	sys.SigningKeys.AddScopedSigner(scoped)

	var results jwt.ValidationResults
	sys.Validate(&results)
	if results.IsBlocking(false) {
		return nil, fmt.Errorf("validation failed: %v", results)
	}

	enc, err := sys.Encode(opKey)
	if err != nil {
		return nil, err
	}

	account := &Account{
		Claims:  *sys,
		Key:     string(accSeed),
		Encoded: enc,
		SigningKeys: []string{
			string(accSigningSeed),
			string(scopedSeed),
		},
	}

	return account, nil
}

func createLatticeAccount(opKey nkeys.KeyPair, importPubKey string, importSigningKey nkeys.KeyPair) (*Account, error) {
	accKey, err := nkeys.CreateAccount()
	if err != nil {
		return nil, err
	}
	accPubKey, err := accKey.PublicKey()
	if err != nil {
		return nil, err
	}
	accSeed, err := accKey.Seed()
	if err != nil {
		return nil, err
	}
	accSigningKey, err := nkeys.CreateAccount()
	if err != nil {
		return nil, err
	}
	accSigningPubKey, err := accSigningKey.PublicKey()
	if err != nil {
		return nil, err
	}
	accSigningSeed, err := accSigningKey.Seed()
	if err != nil {
		return nil, err
	}

	latticeClaim := jwt.NewAccountClaims(accPubKey)
	latticeClaim.Name = "default"
	latticeClaim.SigningKeys.Add(accSigningPubKey)
	// NATS tags are : delimited
	latticeClaim.Tags.Add("accounts.wasmcloud.dev/lattice-id:default")

	latticeClaim.Imports = []*jwt.Import{
		{
			Account: importPubKey,
			Subject: jwt.Subject(fmt.Sprintf("%s.wasmbus.cfg.%s.>", accPubKey, latticeClaim.Name)),
			//Subject: jwt.Subject(fmt.Sprintf("wasmbus.cfg.%s.>", latticeClaim.Name)),
			LocalSubject: jwt.RenamingSubject(fmt.Sprintf("wasmbus.cfg.%s.>", latticeClaim.Name)),
			Type:         jwt.Service,
		},
		{
			Account: importPubKey,
			//Subject: jwt.Subject(fmt.Sprintf("wasmbus.evt.%s.>", latticeClaim.Name)),
			Subject:      jwt.Subject(fmt.Sprintf("%s.wasmbus.evt.%s.>", accPubKey, latticeClaim.Name)),
			LocalSubject: jwt.RenamingSubject(fmt.Sprintf("wasmbus.evt.%s.>", latticeClaim.Name)),
			Type:         jwt.Service,
		},
		{
			Account: importPubKey,
			//Subject: jwt.Subject(fmt.Sprintf("wasmbus.ctl.*.%s.>", latticeClaim.Name)),
			Subject:      jwt.Subject(fmt.Sprintf("%s.wasmbus.ctl.*.%s.>", accPubKey, latticeClaim.Name)),
			LocalSubject: jwt.RenamingSubject(fmt.Sprintf("wasmbus.ctl.*.%s.>", latticeClaim.Name)),
			Type:         jwt.Service,
		},
		{
			Account: importPubKey,
			//Subject: jwt.Subject(fmt.Sprintf("wadm.api.%s.>", latticeClaim.Name)),
			//Subject:      jwt.Subject(fmt.Sprintf("%s.wadm.api.%s.>", accPubKey, latticeClaim.Name)),
			Subject:      jwt.Subject(fmt.Sprintf("wadm.api.%s.%s.>", accPubKey, latticeClaim.Name)),
			LocalSubject: jwt.RenamingSubject(fmt.Sprintf("wadm.api.%s.>", latticeClaim.Name)),
			Type:         jwt.Service,
		},
		{
			Account: importPubKey,
			//Subject: jwt.Subject("wasmcloud.secrets.>"),
			Subject:      jwt.Subject(fmt.Sprintf("%s.wasmcloud.secrets.>", accPubKey)),
			LocalSubject: jwt.RenamingSubject("wasmcloud.secrets.>"),
			Type:         jwt.Service,
		},
		{
			Account: importPubKey,
			Subject: jwt.Subject("wasmcloud.lattice.list"),
			Type:    jwt.Service,
		},
		{
			Account: importPubKey,
			Subject: jwt.Subject("wasmcloud.lattice.watch"),
			Type:    jwt.Stream,
		},
	}

	// Activiation JWTs since the imports are from a different account and are
	// private.
	for _, imp := range latticeClaim.Imports {
		activation := jwt.NewActivationClaims(accPubKey)
		activation.Name = string(imp.Subject)
		activation.IssuerAccount = importPubKey
		activation.ImportType = imp.Type
		activation.ImportSubject = imp.Subject
		// TODO this is actually a bug -- NATS should disallow the import, right?
		//enc, err := activation.Encode(accSigningKey)
		enc, err := activation.Encode(importSigningKey)
		if err != nil {
			return nil, err
		}
		imp.Token = enc
	}

	// Scoped signers:
	// 1. User: meant for human users accessing the lattice. Likely to be
	// superseded by decdicated accounts per user, but this illustrates what we'd
	// need for a particular lattice
	// 2. RPC: generated by hosts on demand for handling RPC traffic for
	// providers and components. Only allows for wRPC calls and potentially
	// Jetstream traffic if needed for providers
	// TODO figure out what other subjects providers need access to.
	scopedKey, err := nkeys.CreateAccount()
	if err != nil {
		return nil, err
	}
	scopedPubKey, err := scopedKey.PublicKey()
	if err != nil {
		return nil, err
	}
	scopedSeed, err := scopedKey.Seed()
	if err != nil {
		return nil, err
	}

	scoped := jwt.NewUserScope()
	scoped.Key = scopedPubKey
	scoped.Role = "user"
	scoped.Template = jwt.UserPermissionLimits{
		Permissions: jwt.Permissions{
			Pub: jwt.Permission{
				Allow: jwt.StringList{
					"wasmbus.cfg.{{account-name()}}.>",
					"wasmbus.evt.{{account-name()}}.>",
					//"wasmbus.ctl.*.{{account-name()}}.>",
					fmt.Sprintf("wasmbus.ctl.*.%s.>", latticeClaim.Name),
					"wadm.api.{{account-name()}}.>",
					"wasmcloud.secrets.>",
					"wasmcloud.lattice.list",
					"_INBOX.>",
				},
			},
			Sub: jwt.Permission{
				Allow: jwt.StringList{
					"wasmbus.cfg.{{account-name()}}.>",
					"wasmbus.evt.{{account-name()}}.>",
					"wasmbus.ctl.*.{{account-name()}}.>",
					"wadm.api.{{account-name()}}.>",
					"{{account-name()}}.*.wrpc.>",
					"wasmcloud.lattice.watch",
					"_INBOX.>",
				},
			},
			Resp: &jwt.ResponsePermission{
				MaxMsgs: -1,
			},
		},
	}
	latticeClaim.SigningKeys.AddScopedSigner(scoped)

	rpcScopedKey, err := nkeys.CreateAccount()
	if err != nil {
		return nil, err
	}
	rpcScopedPubKey, err := rpcScopedKey.PublicKey()
	if err != nil {
		return nil, err
	}
	rpcScopedSeed, err := rpcScopedKey.Seed()
	if err != nil {
		return nil, err
	}

	rpcScope := jwt.NewUserScope()
	rpcScope.Key = rpcScopedPubKey
	rpcScope.Role = "rpc"
	rpcScope.Template = jwt.UserPermissionLimits{
		// TODO: some apps might need access to Jetstream. Maybe something we'd
		// need to configure down the line
		Permissions: jwt.Permissions{
			Pub: jwt.Permission{
				Allow: jwt.StringList{
					"{{account-name()}}.*.wrpc.>",
					"_INBOX.>",
					"$JS.>",
				},
			},
			Sub: jwt.Permission{
				Allow: jwt.StringList{
					"{{account-name()}}.*.wrpc.>",
					"_INBOX.>",
					"$JS.>",
				},
			},
			Resp: &jwt.ResponsePermission{
				MaxMsgs: -1,
			},
		},
	}
	latticeClaim.SigningKeys.AddScopedSigner(rpcScope)

	var results jwt.ValidationResults
	latticeClaim.Validate(&results)
	if results.IsBlocking(false) {
		slog.Info(fmt.Sprintf("%v", latticeClaim))
		return nil, fmt.Errorf("validation failed to create lattice account: %v", results)
	}

	enc, err := latticeClaim.Encode(opKey)
	if err != nil {
		return nil, err
	}

	account := &Account{
		Claims:  *latticeClaim,
		Key:     string(accSeed),
		Encoded: enc,
		SigningKeys: []string{
			string(accSigningSeed),
			string(scopedSeed),
			string(rpcScopedSeed),
		},
	}

	return account, nil
}

func createSystemAccount(opKey nkeys.KeyPair) (*Account, error) {
	accKey, err := nkeys.CreateAccount()
	if err != nil {
		return nil, err
	}
	accPubKey, err := accKey.PublicKey()
	if err != nil {
		return nil, err
	}
	accSeed, err := accKey.Seed()
	if err != nil {
		return nil, err
	}
	accSigningKey, err := nkeys.CreateAccount()
	if err != nil {
		return nil, err
	}
	accSigningPubKey, err := accSigningKey.PublicKey()
	if err != nil {
		return nil, err
	}
	accSigningSeed, err := accSigningKey.Seed()
	if err != nil {
		return nil, err
	}

	sysAccClaim := jwt.NewAccountClaims(accPubKey)
	sysAccClaim.Name = "SYS"
	sysAccClaim.SigningKeys.Add(accSigningPubKey)
	sysAccClaim.Exports = jwt.Exports{&jwt.Export{
		Name:                 "account-monitoring-services",
		Subject:              "$SYS.REQ.ACCOUNT.*.*",
		Type:                 jwt.Service,
		ResponseType:         jwt.ResponseTypeStream,
		AccountTokenPosition: 4,
		Info: jwt.Info{
			Description: `Request account specific monitoring services for: SUBSZ, CONNZ, LEAFZ, JSZ and INFO`,
			InfoURL:     "https://docs.nats.io/nats-server/configuration/sys_accounts",
		},
	}, &jwt.Export{
		Name:                 "account-monitoring-streams",
		Subject:              "$SYS.ACCOUNT.*.>",
		Type:                 jwt.Stream,
		AccountTokenPosition: 3,
		Info: jwt.Info{
			Description: `Account specific monitoring stream`,
			InfoURL:     "https://docs.nats.io/nats-server/configuration/sys_accounts",
		},
	}}

	var results jwt.ValidationResults
	sysAccClaim.Validate(&results)
	if results.IsBlocking(false) {
		return nil, fmt.Errorf("validation failed: %v", results)
	}

	enc, err := sysAccClaim.Encode(opKey)
	if err != nil {
		return nil, err
	}

	account := &Account{
		Claims:  *sysAccClaim,
		Key:     string(accSeed),
		Encoded: enc,
		SigningKeys: []string{
			string(accSigningSeed),
		},
	}

	return account, nil
}

type AuthCallout struct {
	Account          Account
	AuthUser         User
	RegistrationUser User
	XKey             string
}

func authCallout(opKey nkeys.KeyPair, authorizedAccountPubKey string, name string) (*AuthCallout, error) {
	accKey, err := nkeys.CreateAccount()
	if err != nil {
		return nil, err
	}
	accPubKey, err := accKey.PublicKey()
	if err != nil {
		return nil, err
	}
	accSeed, err := accKey.Seed()
	if err != nil {
		return nil, err
	}
	accSigningKey, err := nkeys.CreateAccount()
	if err != nil {
		return nil, err
	}
	accSigningPubKey, err := accSigningKey.PublicKey()
	if err != nil {
		return nil, err
	}
	accSigningSeed, err := accSigningKey.Seed()
	if err != nil {
		return nil, err
	}
	xkey, err := nkeys.CreateCurveKeys()
	if err != nil {
		return nil, err
	}
	xPub, err := xkey.PublicKey()
	if err != nil {
		return nil, err
	}
	xkeySeed, err := xkey.Seed()
	if err != nil {
		return nil, err
	}

	user, err := nkeys.CreateUser()
	if err != nil {
		return nil, err
	}
	userSeed, err := user.Seed()
	if err != nil {
		return nil, err
	}
	userPub, err := user.PublicKey()
	if err != nil {
		return nil, err
	}

	userClaim := jwt.NewUserClaims(userPub)
	userClaim.Name = fmt.Sprintf("%s-writer", name)
	userClaim.IssuerAccount = accPubKey

	var results jwt.ValidationResults
	userClaim.Validate(&results)
	if results.IsBlocking(false) {
		return nil, fmt.Errorf("validation failed: %v", results)
	}

	userEnc, err := userClaim.Encode(accSigningKey)
	if err != nil {
		return nil, err
	}

	authClaim := jwt.NewAccountClaims(accPubKey)
	authClaim.Name = name
	authClaim.SigningKeys.Add(accSigningPubKey)
	authClaim.Authorization.XKey = xPub
	authClaim.Authorization.AuthUsers.Add(userPub)
	authClaim.Authorization.AllowedAccounts.Add(authorizedAccountPubKey)
	authClaim.Limits.DisallowBearer = true
	authClaim.Limits.JetStreamLimits.Consumer = -1
	authClaim.Limits.JetStreamLimits.DiskStorage = -1
	authClaim.Limits.JetStreamLimits.MemoryStorage = -1
	authClaim.Limits.JetStreamLimits.Streams = -1
	authClaim.Limits.JetStreamLimits.MaxAckPending = -1
	authClaim.Limits.JetStreamLimits.DiskMaxStreamBytes = -1
	authClaim.Limits.JetStreamLimits.MemoryMaxStreamBytes = -1

	results = jwt.ValidationResults{}
	authClaim.Validate(&results)
	if results.IsBlocking(false) {
		return nil, fmt.Errorf("validation failed: %v", results)
	}

	enc, err := authClaim.Encode(opKey)
	if err != nil {
		return nil, err
	}

	reg, err := nkeys.CreateUser()
	if err != nil {
		return nil, err
	}
	regPub, err := reg.PublicKey()
	if err != nil {
		return nil, err
	}
	regSeed, err := reg.Seed()
	if err != nil {
		return nil, err
	}

	regClaim := jwt.NewUserClaims(regPub)
	regClaim.Name = fmt.Sprintf("%s-registration", name)
	regClaim.IssuerAccount = accPubKey
	regClaim.Permissions.Pub.Deny.Add(">")
	regClaim.Permissions.Sub.Deny.Add(">")

	results = jwt.ValidationResults{}
	regClaim.Validate(&results)
	if results.IsBlocking(false) {
		return nil, fmt.Errorf("validation failed: %v", results)
	}

	regEnc, err := regClaim.Encode(accSigningKey)
	if err != nil {
		return nil, err
	}
	callout := &AuthCallout{
		Account: Account{
			Claims: *authClaim,
			Key:    string(accSeed),
			SigningKeys: []string{
				string(accSigningSeed),
			},
			Encoded: enc,
		},
		AuthUser: User{
			Claims:  *userClaim,
			Key:     string(userSeed),
			Encoded: userEnc,
		},
		RegistrationUser: User{
			Claims:  *regClaim,
			Key:     string(regSeed),
			Encoded: regEnc,
		},
		XKey: string(xkeySeed),
	}

	return callout, nil
}

// All this code copied from NSC
type NatsResolverConfigBuilder struct {
	operator       string
	operatorName   string
	sysAccountSubj string
	sysAccount     string
	sysAccountName string
	cache          bool
}

func NewNatsResolverConfigBuilder(cache bool) *NatsResolverConfigBuilder {
	cb := NatsResolverConfigBuilder{cache: cache}
	return &cb
}

func (cb *NatsResolverConfigBuilder) Add(rawClaim []byte) error {
	token := string(rawClaim)
	gc, err := jwt.DecodeGeneric(token)
	if err != nil {
		return err
	}
	switch gc.ClaimType() {
	case jwt.OperatorClaim:
		if claim, err := jwt.DecodeOperatorClaims(token); err != nil {
			return err
		} else {
			cb.operator = token
			cb.operatorName = claim.Name
		}
	case jwt.AccountClaim:
		if claim, err := jwt.DecodeAccountClaims(token); err != nil {
			return err
		} else if claim.Subject == cb.sysAccountSubj {
			cb.sysAccount = token
			cb.sysAccountName = claim.Name
		}
	}
	return nil
}

func (cb *NatsResolverConfigBuilder) SetSystemAccount(id string) error {
	cb.sysAccountSubj = id
	return nil
}

const tmplPreLoad = `
# Preload the nats based resolver with the system account jwt.
# This is not necessary but avoids a bootstrapping system account.
# This only applies to the system account. Therefore other account jwt are not included here.
# To populate the resolver:
# 1) make sure that your operator has the account server URL pointing at your nats servers.
#    The url must start with: "nats://"
#    nsc edit operator --account-jwt-server-url nats://localhost:4222
# 2) push your accounts using: nsc push --all
#    The argument to push -u is optional if your account server url is set as described.
# 3) to prune accounts use: nsc push --prune
#    In order to enable prune you must set above allow_delete to true
# Later changes to the system account take precedence over the system account jwt listed here.
resolver_preload: {
	%s: %s,
}
`

const tmplFull = `# Operator named %s
operator: %s
# System Account named %s
system_account: %s

# configuration of the nats based resolver
resolver {
    type: full
    # Directory in which the account jwt will be stored
    dir: './jwt'
    # In order to support jwt deletion, set to true
    # If the resolver type is full delete will rename the jwt.
    # This is to allow manual restoration in case of inadvertent deletion.
    # To restore a jwt, remove the added suffix .delete and restart or send a reload signal.
    # To free up storage you must manually delete files with the suffix .delete.
    allow_delete: true
    # Interval at which a nats-server with a nats based account resolver will compare
    # it's state with one random nats based account resolver in the cluster and if needed,
    # exchange jwt and converge on the same set of jwt.
    interval: "2m"
    # Timeout for lookup requests in case an account does not exist locally.
    timeout: "1.9s"
}

%s
`

const tmplCache = `# Operator named %s
operator: %s
# System Account named %s
system_account: %s

# configuration of the nats based cache resolver
resolver {
    type: cache
    # Directory in which the account jwt will be stored
    dir: './jwt'
    # ttl after which the file will be removed from the cache. Set to a large value in order to disable.
    ttl: "1h"
    # Timeout for lookup requests in case an account does not exist locally.
    timeout: "1.9s"
}

%s
`

func (cb *NatsResolverConfigBuilder) Generate() ([]byte, error) {
	if cb.operator == "" {
		return nil, errors.New("operator is not set")
	}
	if cb.sysAccountSubj == "" || cb.sysAccount == "" {
		return nil, errors.New("system account is not set")
	}
	tmpl := tmplFull
	if cb.cache {
		tmpl = tmplCache
	}
	return []byte(fmt.Sprintf(tmpl, cb.operatorName, cb.operator, cb.sysAccountName, cb.sysAccountSubj,
		fmt.Sprintf(tmplPreLoad, cb.sysAccountSubj, cb.sysAccount))), nil
}
