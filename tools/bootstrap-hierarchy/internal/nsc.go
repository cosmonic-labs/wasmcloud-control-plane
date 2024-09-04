package internal

import (
	"fmt"
	"os"
	"os/exec"
	"path"
	"path/filepath"
	"strings"

	"github.com/nats-io/jwt/v2"
)

const (
	keysDir  = "keys"
	storeDir = "store"
)

type NatsCredentialsDir string

func (n NatsCredentialsDir) KeyDir() string {
	return path.Join(string(n), keysDir)
}

func (n NatsCredentialsDir) StoreDir() string {
	return path.Join(string(n), storeDir)
}

type Operator struct {
	Claims      jwt.OperatorClaims `json:"claims"`
	Encoded     string             `json:"encoded"`
	Key         string             `json:"key"`
	SigningKeys []string           `json:"signing_keys"`
}

func (o *Operator) writeToDir(outDir string) error {
	if err := os.MkdirAll(outDir, 0755); err != nil {
		return err
	}
	if err := os.WriteFile(outDir+"/operator.jwt", []byte(o.Encoded), 0644); err != nil {
		return err
	}
	if err := os.WriteFile(outDir+"/keys/operator.nk", []byte(o.Key), 0644); err != nil {
		return err
	}
	if err := os.WriteFile(outDir+"/keys/operator-signing.nk", []byte(o.SigningKeys[0]), 0644); err != nil {
		return err
	}
	return nil
}

type Account struct {
	Claims      jwt.AccountClaims `json:"claims"`
	Encoded     string            `json:"encoded"`
	Key         string            `json:"key"`
	SigningKeys []string          `json:"signing_keys"`
}

func (a *Account) writeToDir(outDir string) error {
	if err := os.WriteFile(outDir+fmt.Sprintf("/accounts/%s.jwt", a.Claims.Name), []byte(a.Encoded), 0644); err != nil {
		return err
	}
	if err := os.WriteFile(outDir+fmt.Sprintf("/keys/%s.nk", a.Claims.Name), []byte(a.Key), 0644); err != nil {
		return err
	}

	for i, k := range a.SigningKeys {
		if err := os.WriteFile(outDir+fmt.Sprintf("/keys/%s-signing_%d.nk", a.Claims.Name, i), []byte(k), 0644); err != nil {
			return err
		}
	}
	return nil
}

type User struct {
	Claims  jwt.UserClaims `json:"claims"`
	Encoded string         `json:"encoded"`
	Key     string         `json:"key"`
}

func (u *User) writeToDir(outDir string) error {
	if err := os.WriteFile(outDir+fmt.Sprintf("/users/%s.jwt", u.Claims.Name), []byte(u.Encoded), 0644); err != nil {
		return err
	}
	if err := os.WriteFile(outDir+fmt.Sprintf("/keys/%s.nk", u.Claims.Name), []byte(u.Key), 0644); err != nil {
		return err
	}
	return nil
}

type Credentials struct {
	Operator Operator
	Accounts map[string]Account
	Users    map[string]User
}

func NewCredentials() Credentials {
	return Credentials{
		Operator: Operator{},
		Accounts: map[string]Account{},
		Users:    map[string]User{},
	}
}

func (c *Credentials) JWTMap() map[string]string {
	jwts := map[string]string{
		fmt.Sprintf("%s.jwt", c.Operator.Claims.Subject): c.Operator.Encoded,
	}
	for _, account := range c.Accounts {
		jwts[fmt.Sprintf("%s.jwt", account.Claims.Subject)] = account.Encoded
	}
	for _, user := range c.Users {
		jwts[fmt.Sprintf("%s.jwt", user.Claims.Subject)] = user.Encoded
	}
	return jwts
}

type NSC struct {
	CredentialsDir NatsCredentialsDir
	Credentials    Credentials
	KeyStore       map[string]string
}

func (n *NSC) OperatorName() string {
	return n.Credentials.Operator.Claims.Name
}

func New(credsDir NatsCredentialsDir) (*NSC, error) {
	keys := map[string]string{}
	err := filepath.Walk(credsDir.KeyDir(), func(path string, info os.FileInfo, err error) error {
		if filepath.Ext(path) == ".nk" {
			name := filepath.Base(path)[0 : len(filepath.Base(path))-3]
			key, err := os.ReadFile(path)
			if err != nil {
				return err
			}
			keys[name] = string(key)
		}
		return nil
	})
	if err != nil {
		return nil, err
	}

	credentials, err := loadCredentials(credsDir, keys)
	if err != nil {
		return nil, err
	}

	return &NSC{
		CredentialsDir: credsDir,
		Credentials:    *credentials,
		KeyStore:       keys,
	}, nil
}

func loadCredentials(credsDir NatsCredentialsDir, keys map[string]string) (*Credentials, error) {
	credentials := NewCredentials()

	jwts := map[string]string{}
	_ = filepath.Walk(credsDir.StoreDir(), func(path string, info os.FileInfo, err error) error {
		if filepath.Ext(path) == ".jwt" {
			j, err := os.ReadFile(path)
			if err != nil {
				return err
			}
			claims, err := jwt.Decode(string(j))
			if err != nil {
				return err
			}
			jwts[claims.Claims().Subject] = string(j)
		}
		return nil
	})

	if len(jwts) == 0 {
		return nil, fmt.Errorf("no JWTs found in %s", credsDir.StoreDir())
	}

	var operatorID string
	for key := range jwts {
		if strings.HasPrefix(key, "O") {
			operatorID = key
			break
		}
	}

	if operatorID == "" {
		return nil, fmt.Errorf("no operator JWT found in %s", credsDir.StoreDir())
	}

	// TODO error check
	operator, _ := jwt.DecodeOperatorClaims(jwts[operatorID])
	credentials.Operator = Operator{
		Claims:  *operator,
		Key:     keys[operatorID],
		Encoded: jwts[operatorID],
	}

	for _, k := range operator.SigningKeys {
		credentials.Operator.SigningKeys = append(credentials.Operator.SigningKeys, keys[k])
	}

	for key, value := range jwts {
		if strings.HasPrefix(key, "A") {
			account, err := jwt.DecodeAccountClaims(value)
			if err != nil {
				return nil, err
			}
			accounts := Account{
				Claims:  *account,
				Key:     keys[key],
				Encoded: value,
			}
			for k := range account.SigningKeys {
				accounts.SigningKeys = append(accounts.SigningKeys, keys[k])
			}
			credentials.Accounts[account.Name] = accounts
		}

		if strings.HasPrefix(key, "U") {
			user, err := jwt.DecodeUserClaims(value)
			if err != nil {
				return nil, err
			}
			credentials.Users[user.Name] = User{
				Claims:  *user,
				Key:     keys[key],
				Encoded: value,
			}
		}
	}

	return &credentials, nil
}

func (c *NSC) RunCommand(args ...string) (string, error) {
	ctxArgs := []string{
		"--config-dir", string(c.CredentialsDir),
		"--data-dir", c.CredentialsDir.StoreDir(),
		"--keystore-dir", c.CredentialsDir.KeyDir(),
	}
	args = append(args, ctxArgs...)
	return c.runCommand(args...)
}

func (c *NSC) runCommand(args ...string) (string, error) {
	fmt.Println(args)
	command := "nsc"
	cmd := exec.Command(command, args...)

	stdout, err := cmd.CombinedOutput()
	if err != nil {
		if ee, ok := err.(*exec.ExitError); ok {
			return "", fmt.Errorf("%s", ee.Stderr)
		}
		return "", err
	}

	return string(stdout), err
}
