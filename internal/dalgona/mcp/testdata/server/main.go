package main

import (
	"bufio"
	"bytes"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"strconv"
	"time"
)

const echoSchema = `{"type":"object","properties":{"text":{"type":"string"}},"required":["text"],"x-dalgona-passthrough":true}`

func main() {
	if err := run(); err != nil {
		_, _ = fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func run() error {
	if os.Getenv("MCP_TEST_HOLD_STDOUT") != "" {
		time.Sleep(30 * time.Second)
		return nil
	}
	if err := recordStart(); err != nil {
		return err
	}
	if pidFile := os.Getenv("GRANDCHILD_PID_FILE"); pidFile != "" {
		if err := spawnStdoutHolder(pidFile); err != nil {
			return err
		}
	}
	scanner := bufio.NewScanner(os.Stdin)
	scanner.Buffer(make([]byte, 4096), 16<<20)
	writer := bufio.NewWriter(os.Stdout)
	for scanner.Scan() {
		fields, err := decodeObject(scanner.Bytes())
		if err != nil {
			return err
		}
		method, err := decodeString(fields["method"])
		if err != nil {
			return err
		}
		if method == "notifications/initialized" {
			continue
		}
		result, dispatchErr := dispatch(method, fields["params"])
		if dispatchErr != nil {
			if failure, ok := errors.AsType[*rpcFailure](dispatchErr); ok {
				if err := writeErrorResponse(writer, fields["id"], failure); err != nil {
					return err
				}
				continue
			}
			return dispatchErr
		}
		if err := writeResponse(writer, fields["id"], result); err != nil {
			return err
		}
	}
	return scanner.Err()
}

type rpcFailure struct {
	Code    int
	Message string
}

func (e *rpcFailure) Error() string {
	return e.Message
}

func recordStart() error {
	path := os.Getenv("START_LOG")
	if path == "" {
		return nil
	}
	file, err := os.OpenFile(path, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o600)
	if err != nil {
		return fmt.Errorf("open start log: %w", err)
	}
	if _, err := file.WriteString("start\n"); err != nil {
		_ = file.Close()
		return fmt.Errorf("write start log: %w", err)
	}
	if err := file.Close(); err != nil {
		return fmt.Errorf("close start log: %w", err)
	}
	return nil
}
func spawnStdoutHolder(pidFile string) error {
	exe, err := os.Executable()
	if err != nil {
		return err
	}
	child := exec.Command(exe)
	child.Env = append(os.Environ(), "MCP_TEST_HOLD_STDOUT=1")
	child.Stdout = os.Stdout
	if err := child.Start(); err != nil {
		return err
	}
	pid := child.Process.Pid
	_ = child.Process.Release()
	if err := os.WriteFile(pidFile, []byte(strconv.Itoa(pid)), 0o600); err != nil {
		return err
	}
	return nil
}

func dispatch(method string, params []byte) (jsontext.Value, error) {
	switch method {
	case "initialize":
		return jsontext.Value(`{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"stdio-test","version":"1"}}`), nil
	case "tools/list":
		return listTools(params)
	case "tools/call":
		return callTool(params)
	default:
		return nil, fmt.Errorf("unexpected method %s", method)
	}
}

func listTools(params []byte) (jsontext.Value, error) {
	fields, err := decodeObject(params)
	if err != nil {
		return nil, err
	}
	cursor := ""
	if raw, ok := fields["cursor"]; ok {
		cursor, err = decodeString(raw)
		if err != nil {
			return nil, err
		}
	}
	if cursor == "page2" {
		return jsontext.Value(`{"tools":[{"name":"other","description":"Other tool","inputSchema":{"type":"object"}}]}`), nil
	}
	return jsontext.Value(`{"tools":[{"name":"echo","description":"Echo text","inputSchema":` + echoSchema + `}],"nextCursor":"page2"}`), nil
}

func callTool(params []byte) (jsontext.Value, error) {
	fields, err := decodeObject(params)
	if err != nil {
		return nil, err
	}
	name, err := decodeString(fields["name"])
	if err != nil || name != "echo" {
		return nil, errors.New("unknown tool")
	}
	arguments, err := decodeObject(fields["arguments"])
	if err != nil {
		return nil, err
	}
	text, err := decodeString(arguments["text"])
	if err != nil {
		return nil, err
	}
	switch text {
	case "always":
		os.Exit(7)
	case "crash":
		if err := crashOnce(); err != nil {
			return nil, err
		}
		text = "recovered"
	case "error":
		return nil, &rpcFailure{Code: -32602, Message: "Unknown tool: error"}
	case "fail":
		return jsontext.Value(`{"content":[{"type":"text","text":"echo failed"}],"isError":true}`), nil
	case "blocks":
		return jsontext.Value(`{"content":[{"type":"text","text":"hello"},{"type":"image","data":"aGVsbG8=","mimeType":"image/png"},{"type":"resource_link","uri":"file:///project/src/main.rs","name":"main.rs"}]}`), nil
	}
	return textResult(text)
}

func crashOnce() error {
	path := os.Getenv("CRASH_FILE")
	if path == "" {
		os.Exit(7)
	}
	if _, err := os.Stat(path); err == nil {
		return nil
	}
	if err := os.WriteFile(path, []byte("crashed"), 0o600); err != nil {
		return fmt.Errorf("write crash marker: %w", err)
	}
	os.Exit(7)
	return nil
}

func textResult(text string) (jsontext.Value, error) {
	var out bytes.Buffer
	encoder := jsontext.NewEncoder(&out)
	var writeErr error
	write := func(token jsontext.Token) {
		if writeErr == nil {
			writeErr = encoder.WriteToken(token)
		}
	}
	write(jsontext.BeginObject)
	write(jsontext.String("content"))
	write(jsontext.BeginArray)
	write(jsontext.BeginObject)
	write(jsontext.String("type"))
	write(jsontext.String("text"))
	write(jsontext.String("text"))
	write(jsontext.String(text))
	write(jsontext.EndObject)
	write(jsontext.EndArray)
	write(jsontext.String("isError"))
	write(jsontext.Bool(false))
	write(jsontext.EndObject)
	if writeErr != nil {
		return nil, fmt.Errorf("encode text result: %w", writeErr)
	}
	return bytes.TrimSuffix(out.Bytes(), []byte{'\n'}), nil
}

func writeResponse(writer *bufio.Writer, id, result []byte) error {
	var out bytes.Buffer
	encoder := jsontext.NewEncoder(&out)
	if err := encoder.WriteToken(jsontext.BeginObject); err != nil {
		return err
	}
	if err := writeStringMember(encoder, "jsonrpc", "2.0"); err != nil {
		return err
	}
	if err := encoder.WriteToken(jsontext.String("id")); err != nil {
		return err
	}
	if err := encoder.WriteValue(id); err != nil {
		return err
	}
	if err := encoder.WriteToken(jsontext.String("result")); err != nil {
		return err
	}
	if err := encoder.WriteValue(result); err != nil {
		return err
	}
	if err := encoder.WriteToken(jsontext.EndObject); err != nil {
		return err
	}
	message := append(bytes.TrimSuffix(out.Bytes(), []byte{'\n'}), '\n')
	if _, err := writer.Write(message); err != nil {
		return err
	}
	return writer.Flush()
}

func writeStringMember(encoder *jsontext.Encoder, name, value string) error {
	if err := encoder.WriteToken(jsontext.String(name)); err != nil {
		return err
	}
	return encoder.WriteToken(jsontext.String(value))
}

func writeErrorResponse(writer *bufio.Writer, id []byte, failure *rpcFailure) error {
	var out bytes.Buffer
	encoder := jsontext.NewEncoder(&out)
	if err := encoder.WriteToken(jsontext.BeginObject); err != nil {
		return err
	}
	if err := writeStringMember(encoder, "jsonrpc", "2.0"); err != nil {
		return err
	}
	if err := encoder.WriteToken(jsontext.String("id")); err != nil {
		return err
	}
	if err := encoder.WriteValue(id); err != nil {
		return err
	}
	if err := encoder.WriteToken(jsontext.String("error")); err != nil {
		return err
	}
	if err := encoder.WriteToken(jsontext.BeginObject); err != nil {
		return err
	}
	if err := encoder.WriteToken(jsontext.String("code")); err != nil {
		return err
	}
	if err := encoder.WriteToken(jsontext.Int(int64(failure.Code))); err != nil {
		return err
	}
	if err := writeStringMember(encoder, "message", failure.Message); err != nil {
		return err
	}
	if err := encoder.WriteToken(jsontext.EndObject); err != nil {
		return err
	}
	if err := encoder.WriteToken(jsontext.EndObject); err != nil {
		return err
	}
	if _, err := writer.Write(out.Bytes()); err != nil {
		return err
	}
	return writer.Flush()
}

func decodeObject(raw []byte) (map[string]jsontext.Value, error) {
	decoder := jsontext.NewDecoder(bytes.NewReader(raw))
	start, err := decoder.ReadToken()
	if err != nil {
		return nil, err
	}
	if start.Kind() != jsontext.KindBeginObject {
		return nil, errors.New("expected object")
	}
	fields := make(map[string]jsontext.Value)
	for decoder.PeekKind() != jsontext.KindEndObject {
		name, err := decoder.ReadToken()
		if err != nil {
			return nil, err
		}
		fieldName := name.String()
		value, err := decoder.ReadValue()
		if err != nil {
			return nil, err
		}
		end := decoder.InputOffset()
		fields[fieldName] = raw[end-int64(len(value)) : end]
	}
	if _, err := decoder.ReadToken(); err != nil {
		return nil, err
	}
	return fields, nil
}

func decodeString(raw []byte) (string, error) {
	decoder := jsontext.NewDecoder(bytes.NewReader(raw))
	token, err := decoder.ReadToken()
	if err != nil {
		return "", err
	}
	if token.Kind() != jsontext.KindString {
		return "", errors.New("expected string")
	}
	value := token.String()
	if _, err := decoder.ReadToken(); !errors.Is(err, io.EOF) {
		return "", errors.New("trailing JSON")
	}
	return value, nil
}
