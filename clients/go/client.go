// Package propfirm provides a typed Go client for the propfirm-engine
// HTTP API. It wraps the /internal/v1/evaluate endpoint with proper
// request/response types, idempotency-key handling, and tenant
// header propagation.
//
// Usage:
//
//	client := propfirm.New("http://propfirm-server:8080", "my-tenant-uuid")
//	resp, err := client.Evaluate(ctx, &propfirm.EvaluateRequest{
//	    AccountID:    "uuid-here",
//	    AccountState: account,
//	    BridgeTick:   &propfirm.BridgeTick{...},
//	})
package propfirm

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"time"

	"github.com/google/uuid"
)

// Client is the propfirm-engine HTTP client.
type Client struct {
	baseURL    string
	tenantID   string
	httpClient *http.Client
}

// New creates a new propfirm-engine client.
// baseURL is the engine's HTTP address (e.g. "http://propfirm-server:8080").
// tenantID is the UUID sent in the X-Tenant-Id header.
func New(baseURL, tenantID string) *Client {
	return &Client{
		baseURL:  baseURL,
		tenantID: tenantID,
		httpClient: &http.Client{
			Timeout: 30 * time.Second,
		},
	}
}

// NewWithHTTPClient allows callers to provide a custom *http.Client
// (e.g. with custom transport, retry logic, or circuit breaker).
func NewWithHTTPClient(baseURL, tenantID string, httpClient *http.Client) *Client {
	return &Client{
		baseURL:    baseURL,
		tenantID:   tenantID,
		httpClient: httpClient,
	}
}

// Health checks the engine's liveness probe.
// Returns nil if the engine is healthy.
func (c *Client) Health(ctx context.Context) error {
	req, err := http.NewRequestWithContext(ctx, "GET", c.baseURL+"/health", nil)
	if err != nil {
		return fmt.Errorf("create request: %w", err)
	}
	resp, err := c.httpClient.Do(req)
	if err != nil {
		return fmt.Errorf("health check: %w", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf("health check returned %d", resp.StatusCode)
	}
	return nil
}

// Ready checks the engine's readiness probe.
func (c *Client) Ready(ctx context.Context) error {
	req, _ := http.NewRequestWithContext(ctx, "GET", c.baseURL+"/ready", nil)
	resp, err := c.httpClient.Do(req)
	if err != nil {
		return fmt.Errorf("readiness check: %w", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf("readiness check returned %d", resp.StatusCode)
	}
	return nil
}

// Metrics scrapes the Prometheus /metrics endpoint.
func (c *Client) Metrics(ctx context.Context) (string, error) {
	req, _ := http.NewRequestWithContext(ctx, "GET", c.baseURL+"/metrics", nil)
	resp, err := c.httpClient.Do(req)
	if err != nil {
		return "", fmt.Errorf("metrics scrape: %w", err)
	}
	defer resp.Body.Close()
	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return "", fmt.Errorf("read metrics body: %w", err)
	}
	return string(body), nil
}

// Evaluate sends a stateless evaluation request to the engine.
// If idempotencyKey is non-empty, it's sent as the Idempotency-Key
// header for deduplication.
func (c *Client) Evaluate(ctx context.Context, req *EvaluateRequest, idempotencyKey string) (*EvaluateResponse, error) {
	body, err := json.Marshal(req)
	if err != nil {
		return nil, fmt.Errorf("marshal request: %w", err)
	}

	httpReq, err := http.NewRequestWithContext(ctx, "POST", c.baseURL+"/internal/v1/evaluate", bytes.NewReader(body))
	if err != nil {
		return nil, fmt.Errorf("create request: %w", err)
	}
	httpReq.Header.Set("Content-Type", "application/json")
	httpReq.Header.Set("X-Tenant-Id", c.tenantID)
	if idempotencyKey != "" {
		httpReq.Header.Set("Idempotency-Key", idempotencyKey)
	}

	resp, err := c.httpClient.Do(httpReq)
	if err != nil {
		return nil, fmt.Errorf("evaluate request: %w", err)
	}
	defer resp.Body.Close()

	respBody, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, fmt.Errorf("read response body: %w", err)
	}

	if resp.StatusCode == http.StatusConflict {
		return nil, fmt.Errorf("idempotency conflict: key %s already used with different body", idempotencyKey)
	}
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("evaluate returned %d: %s", resp.StatusCode, string(respBody))
	}

	var evalResp EvaluateResponse
	if err := json.Unmarshal(respBody, &evalResp); err != nil {
		return nil, fmt.Errorf("unmarshal response: %w", err)
	}
	return &evalResp, nil
}

// EvaluateWithAutoKey calls Evaluate with a random UUID idempotency key.
func (c *Client) EvaluateWithAutoKey(ctx context.Context, req *EvaluateRequest) (*EvaluateResponse, error) {
	return c.Evaluate(ctx, req, uuid.NewString())
}

// EvaluateOrder sends a pre-trade order evaluation request.
func (c *Client) EvaluateOrder(ctx context.Context, req *EvaluateOrderRequest) (*EvaluateOrderResponse, error) {
	body, err := json.Marshal(req)
	if err != nil {
		return nil, fmt.Errorf("marshal request: %w", err)
	}

	httpReq, _ := http.NewRequestWithContext(ctx, "POST", c.baseURL+"/v1/evaluate-order", bytes.NewReader(body))
	httpReq.Header.Set("Content-Type", "application/json")
	httpReq.Header.Set("X-Tenant-Id", c.tenantID)

	resp, err := c.httpClient.Do(httpReq)
	if err != nil {
		return nil, fmt.Errorf("evaluate-order request: %w", err)
	}
	defer resp.Body.Close()

	respBody, _ := io.ReadAll(resp.Body)
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("evaluate-order returned %d: %s", resp.StatusCode, string(respBody))
	}

	var orderResp EvaluateOrderResponse
	if err := json.Unmarshal(respBody, &orderResp); err != nil {
		return nil, fmt.Errorf("unmarshal response: %w", err)
	}
	return &orderResp, nil
}

// Override clears a false-positive breach. The actor_id + reason are
// recorded in the audit_log table.
func (c *Client) Override(ctx context.Context, req *OverrideRequest) (*OverrideResponse, error) {
	body, _ := json.Marshal(req)
	httpReq, _ := http.NewRequestWithContext(ctx, "POST", c.baseURL+"/internal/v1/override", bytes.NewReader(body))
	httpReq.Header.Set("Content-Type", "application/json")
	httpReq.Header.Set("X-Tenant-Id", c.tenantID)

	resp, err := c.httpClient.Do(httpReq)
	if err != nil {
		return nil, fmt.Errorf("override request: %w", err)
	}
	defer resp.Body.Close()

	respBody, _ := io.ReadAll(resp.Body)
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("override returned %d: %s", resp.StatusCode, string(respBody))
	}

	var overrideResp OverrideResponse
	if err := json.Unmarshal(respBody, &overrideResp); err != nil {
		return nil, fmt.Errorf("unmarshal response: %w", err)
	}
	return &overrideResp, nil
}

// EmergencyStop forces an emergency stop on an account.
func (c *Client) EmergencyStop(ctx context.Context, req *EmergencyStopRequest) (*EmergencyStopResponse, error) {
	body, _ := json.Marshal(req)
	httpReq, _ := http.NewRequestWithContext(ctx, "POST", c.baseURL+"/internal/v1/emergency-stop", bytes.NewReader(body))
	httpReq.Header.Set("Content-Type", "application/json")
	httpReq.Header.Set("X-Tenant-Id", c.tenantID)

	resp, err := c.httpClient.Do(httpReq)
	if err != nil {
		return nil, fmt.Errorf("emergency-stop request: %w", err)
	}
	defer resp.Body.Close()

	respBody, _ := io.ReadAll(resp.Body)
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("emergency-stop returned %d: %s", resp.StatusCode, string(respBody))
	}

	var stopResp EmergencyStopResponse
	if err := json.Unmarshal(respBody, &stopResp); err != nil {
		return nil, fmt.Errorf("unmarshal response: %w", err)
	}
	return &stopResp, nil
}

// ValidateRulePack validates a rule pack without persisting it.
func (c *Client) ValidateRulePack(ctx context.Context, rules []RuleEntryDTO) (*RulePackResponse, error) {
	body, _ := json.Marshal(map[string]interface{}{"rules": rules})
	httpReq, _ := http.NewRequestWithContext(ctx, "POST", c.baseURL+"/v1/rule-packs/validate", bytes.NewReader(body))
	httpReq.Header.Set("Content-Type", "application/json")

	resp, err := c.httpClient.Do(httpReq)
	if err != nil {
		return nil, fmt.Errorf("validate rule pack: %w", err)
	}
	defer resp.Body.Close()

	respBody, _ := io.ReadAll(resp.Body)
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("validate returned %d: %s", resp.StatusCode, string(respBody))
	}

	var packResp RulePackResponse
	if err := json.Unmarshal(respBody, &packResp); err != nil {
		return nil, fmt.Errorf("unmarshal response: %w", err)
	}
	return &packResp, nil
}
