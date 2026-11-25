package main

import (
	"bufio"
	"context"
	"crypto/sha256"
	"crypto/tls"
	"crypto/x509"
	"encoding/hex"
	json "encoding/json/v2"
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"strings"
	"time"

	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/daemon"
)

const (
	sshUsage     = "onboard ssh <name> <host-alias-or-user@host> [--artifact <local-path>]"
	pairUsage    = "pair issue <https-origin> --trust-account | join <name> <device-label> --trust-account | list | revoke <device-id>"
	pairingTrust = "A paired device gets full access to the daemon account. It can run commands, read or change files, and create other ways to access the account. Revocation closes its connections and rejects its credential; it does not undo actions or remove other access."
)

func runOnboard(ctx context.Context, args []string, std stdio) int {
	if len(args) != 3 && len(args) != 5 || args[0] != "ssh" {
		return usage(std, sshUsage)
	}
	target := client.SSHTarget{Destination: args[2]}
	if len(args) == 5 {
		if args[3] != "--artifact" || args[4] == "" {
			return usage(std, sshUsage)
		}
		target.Artifact = args[4]
	}
	cfg, err := loadConfig()
	if err != nil {
		return fail(std, 1, err)
	}
	remotes := client.NewRemotes(cfg, nil)
	if _, err := remotes.OnboardSSH(ctx, args[1], target); err != nil {
		return fail(std, 1, err)
	}
	fmt.Fprintf(std.out, "Added SSH machine %q. Connect with dalgona --remote %s or select it in /machines.\n", args[1], args[1])
	return 0
}

func runPairing(ctx context.Context, args []string, std stdio) int {
	if len(args) == 0 {
		return usage(std, pairUsage)
	}
	if std.remote != "" {
		return fail(std, 2, errors.New("pair uses the local product home; omit --remote"))
	}
	switch args[0] {
	case "issue":
		return pairIssue(ctx, args[1:], std)
	case "join":
		return pairJoin(ctx, args[1:], std)
	case "list", "revoke":
		return pairAdmin(ctx, args, std)
	default:
		return usage(std, pairUsage)
	}
}

func pairIssue(ctx context.Context, args []string, std stdio) int {
	if len(args) != 2 || args[1] != "--trust-account" {
		fmt.Fprintln(std.errOut, pairingTrust)
		return usage(std, "pair issue <https-origin> --trust-account")
	}
	fmt.Fprintln(std.errOut, pairingTrust)
	cfg, err := loadConfig()
	if err != nil {
		return fail(std, 1, err)
	}
	card, code, err := issuePairingCode(ctx, cfg, args[0])
	if err != nil {
		return fail(std, 1, err)
	}
	encoded, err := json.Marshal(card)
	if err != nil {
		return fail(std, 1, err)
	}
	fmt.Fprintln(std.errOut, "Send the pairing card and the separate one-use code to the device through a trusted channel. The code expires in five minutes.")
	if _, err := fmt.Fprintf(std.out, "%s\n%s\n", encoded, code); err != nil {
		return fail(std, 1, fmt.Errorf("could not deliver the pairing code; issue a new grant: %w", err))
	}
	return 0
}

func issuePairingCode(ctx context.Context, cfg config.Config, address string) (rpc.PairingCard, string, error) {
	origin, err := config.ParsePairingOrigin(address)
	if err != nil {
		return rpc.PairingCard{}, "", err
	}
	pin, err := relayPublicKey(cfg, origin.Hostname())
	if err != nil {
		return rpc.PairingCard{}, "", err
	}
	reply, err := localMachineRequest(ctx, cfg.Home, rpc.MachineRequest{Action: rpc.MachineGrant})
	if err != nil {
		return rpc.PairingCard{}, "", err
	}
	return rpc.PairingCard{Origin: origin.String(), MachineID: reply.MachineID, PublicKeyPin: pin}, reply.Credential, nil
}

func relayPublicKey(cfg config.Config, hostname string) (string, error) {
	if cfg.Web.CertFile == "" || cfg.Web.KeyFile == "" {
		return "", errors.New("configure [web] cert_file and key_file and start dalgona web explicitly before pairing")
	}
	cert, err := tls.LoadX509KeyPair(cfg.Web.CertFile, cfg.Web.KeyFile)
	if err != nil {
		return "", fmt.Errorf("load relay TLS key pair: %w", err)
	}
	leaf, err := x509.ParseCertificate(cert.Certificate[0])
	if err != nil {
		return "", fmt.Errorf("parse relay certificate: %w", err)
	}
	if err := leaf.VerifyHostname(hostname); err != nil {
		return "", fmt.Errorf("pairing origin does not match the configured relay certificate: %w", err)
	}
	digest := sha256.Sum256(leaf.RawSubjectPublicKeyInfo)
	return hex.EncodeToString(digest[:]), nil
}

func pairJoin(ctx context.Context, args []string, std stdio) int {
	fmt.Fprintln(std.errOut, pairingTrust)
	if len(args) != 3 || args[2] != "--trust-account" {
		return usage(std, "pair join <name> <device-label> --trust-account")
	}
	cfg, err := loadConfig()
	if err != nil {
		return fail(std, 1, err)
	}
	card, code, err := readPairingCard(std)
	if err != nil {
		return fail(std, 1, err)
	}
	remotes := client.NewRemotes(cfg, nil)
	if _, err := remotes.PairDevice(ctx, args[0], card, code, args[1]); err != nil {
		return fail(std, 1, err)
	}
	fmt.Fprintf(std.out, "Added paired machine %q. Open it in /machines or start the TUI with dalgona --remote %s.\n", args[0], args[0])
	return 0
}

func readPairingCard(std stdio) (rpc.PairingCard, string, error) {
	input := bufio.NewReaderSize(std.in, 4096)
	fmt.Fprintln(std.errOut, "Paste the trusted pairing card JSON on the first line, then the separate code on the second line.")
	line, err := input.ReadSlice('\n')
	if err != nil {
		return rpc.PairingCard{}, "", errors.New("pairing card must be one JSON line of at most 4095 bytes")
	}
	var card rpc.PairingCard
	if err = json.Unmarshal(line, &card, json.RejectUnknownMembers(true)); err != nil {
		return card, "", errors.New("invalid pairing card JSON")
	}
	line, err = input.ReadSlice('\n')
	if err != nil && !errors.Is(err, io.EOF) {
		return card, "", errors.New("invalid pairing code input")
	}
	code := strings.TrimSpace(string(line))
	if code == "" || len(code) > 64 {
		return card, "", errors.New("pairing code is missing or invalid")
	}
	return card, code, nil
}

func pairAdmin(ctx context.Context, args []string, std stdio) int {
	if !pairAdminArgsValid(args) {
		return usage(std, "pair list | revoke <device-id>")
	}
	cfg, err := loadConfig()
	if err != nil {
		return fail(std, 1, err)
	}
	if args[0] == "revoke" {
		return revokePairedDevice(ctx, cfg.Home, std, args[1])
	}
	return listPairedDevices(ctx, cfg.Home, std)
}

func pairAdminArgsValid(args []string) bool {
	if args[0] == "list" {
		return len(args) == 1
	}
	if args[0] == "revoke" {
		return len(args) == 2
	}
	return true
}

func listPairedDevices(ctx context.Context, home string, std stdio) int {
	reply, err := localMachineRequest(ctx, home, rpc.MachineRequest{Action: rpc.MachineDevices})
	if err != nil {
		return fail(std, 1, err)
	}
	for _, device := range reply.Devices {
		fmt.Fprintf(std.out, "%s\t%q\t%s\n", device.ID, device.Label, device.CreatedAt.Format(time.RFC3339))
	}
	return 0
}

func revokePairedDevice(ctx context.Context, home string, std stdio, deviceID string) int {
	if _, err := localMachineRequest(ctx, home, rpc.MachineRequest{Action: rpc.MachineRevoke, DeviceID: deviceID}); err != nil {
		return fail(std, 1, err)
	}
	fmt.Fprintln(std.out, "Device revoked. Its credential and connections are disabled. Prior effects and other access are not removed.")
	return 0
}

func localMachineRequest(ctx context.Context, home string, req rpc.MachineRequest) (rpc.MachineReply, error) {
	session, _, err := localSession(home)
	if err != nil {
		return rpc.MachineReply{}, err
	}
	defer func() { _ = session.Close() }()
	conn, err := session.Connect(ctx)
	if err != nil {
		return rpc.MachineReply{}, err
	}
	defer func() { _ = conn.Close() }()
	reply, err := rpc.ExchangeMachine(ctx, conn, req)
	if err != nil {
		return reply, err
	}
	if reply.Status == rpc.MachineMissing {
		return reply, errors.New("no paired device with that ID; run dalgona pair list")
	}
	if reply.Status != "" {
		return reply, errors.New("local daemon cannot process the pairing request; check the device registry and capacity")
	}
	return reply, nil
}

func runMachineBridge(ctx context.Context, args []string, std stdio) int {
	if len(args) > 1 || len(args) == 1 && !config.ValidMachineDigest(args[0]) {
		return usage(std, "bridge --machine [expected-machine-id]")
	}
	expected := ""
	if len(args) == 1 {
		expected = args[0]
	}
	if err := machineBridge(ctx, expected, std); err != nil {
		return fail(std, 1, err)
	}
	return 0
}

func machineBridge(ctx context.Context, expected string, std stdio) error {
	home := config.ProductDalgona.Home()
	stop, err := verifyExpectedMachine(expected, home, std)
	if err != nil {
		return err
	}
	if stop {
		return nil
	}
	session, _, err := localSession(home)
	if err != nil {
		return err
	}
	defer func() { _ = session.Close() }()
	conn, err := session.Connect(ctx)
	if err != nil {
		return err
	}
	defer func() { _ = conn.Close() }()
	return forwardMachineBridge(ctx, conn, expected, std)
}

func verifyExpectedMachine(expected, home string, std stdio) (bool, error) {
	if expected == "" {
		return false, nil
	}
	actual, err := daemon.ReadMachineIdentity(home)
	if err != nil && !errors.Is(err, os.ErrNotExist) {
		return false, err
	}
	if actual != expected {
		return true, rpc.WriteMachineFrame(std, rpc.MachineReply{Version: rpc.MachineVersion, MachineID: actual, Status: rpc.MachineMismatch})
	}
	return false, nil
}

func forwardMachineBridge(ctx context.Context, conn net.Conn, expected string, std stdio) error {
	reply, err := rpc.ExchangeMachine(ctx, conn, rpc.MachineRequest{Action: rpc.MachineSSH, MachineID: expected})
	if err != nil {
		return err
	}
	if err := rpc.WriteMachineFrame(std, reply); err != nil || reply.Status != "" {
		return err
	}
	return splice(ctx, std, conn)
}
