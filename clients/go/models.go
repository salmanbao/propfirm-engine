package propfirm

// EvaluateRequest is the wire shape for POST /internal/v1/evaluate.
type EvaluateRequest struct {
	AccountID         string          `json:"account_id"`
	AccountState      json.RawMessage `json:"account_state,omitempty"`
	BridgeTick        *BridgeTick     `json:"bridge_tick,omitempty"`
	Tick              *TickDTO        `json:"tick,omitempty"`
	OpenPositions     []PositionDTO   `json:"open_positions,omitempty"`
	TodayTrades       []TradeDTO      `json:"today_trades,omitempty"`
	EquitySource      string          `json:"equity_source,omitempty"`
	CrossReferenceTrades []TradeDTO    `json:"cross_reference_trades,omitempty"`
}

// EvaluateResponse is the response from POST /internal/v1/evaluate.
type EvaluateResponse struct {
	Evaluated        bool             `json:"evaluated"`
	DecisionKind     string           `json:"decision_kind"`
	WinningPriority  uint32           `json:"winning_priority"`
	InputHash        string           `json:"input_hash"`
	PackVersion      uint32           `json:"pack_version"`
	PackID           string           `json:"pack_id"`
	Violations       []string         `json:"violations"`
	ViolationDetails json.RawMessage  `json:"violation_details"`
	AccountState     json.RawMessage  `json:"account_state"`
}

// EvaluateOrderRequest is the wire shape for POST /v1/evaluate-order.
type EvaluateOrderRequest struct {
	AccountID    string  `json:"account_id"`
	AccountState json.RawMessage `json:"account_state,omitempty"`
	Symbol       string  `json:"symbol"`
	Side         string  `json:"side"`
	OrderType    string  `json:"order_type"`
	Quantity     string  `json:"quantity"`
	Price        *string `json:"price,omitempty"`
	StopLoss     *string `json:"stop_loss,omitempty"`
	TakeProfit   *string `json:"take_profit,omitempty"`
}

// EvaluateOrderResponse is the response from POST /v1/evaluate-order.
type EvaluateOrderResponse struct {
	Decision        string           `json:"decision"`
	Passed          bool             `json:"passed"`
	Violations      []string         `json:"violations"`
	ViolationDetails json.RawMessage `json:"violation_details"`
	AccountState    json.RawMessage  `json:"account_state"`
}

// OverrideRequest is the wire shape for POST /internal/v1/override.
type OverrideRequest struct {
	AccountID         string          `json:"account_id"`
	AccountState      json.RawMessage `json:"account_state,omitempty"`
	ClearsViolationID string          `json:"clears_violation_id"`
	Reason            string          `json:"reason"`
	ActorID           string          `json:"actor_id"`
	Violation         json.RawMessage `json:"violation"`
}

// OverrideResponse is the response from POST /internal/v1/override.
type OverrideResponse struct {
	OverrideID   string          `json:"override_id"`
	ClearedAt    string          `json:"cleared_at"`
	AccountState json.RawMessage `json:"account_state"`
}

// EmergencyStopRequest is the wire shape for POST /internal/v1/emergency-stop.
type EmergencyStopRequest struct {
	AccountID    string          `json:"account_id"`
	AccountState json.RawMessage `json:"account_state,omitempty"`
	Reason       string          `json:"reason"`
	ActorID      string          `json:"actor_id"`
}

// EmergencyStopResponse is the response from POST /internal/v1/emergency-stop.
type EmergencyStopResponse struct {
	DecisionKind  string          `json:"decision_kind"`
	StoppedAt     string          `json:"stopped_at"`
	AccountState  json.RawMessage `json:"account_state"`
}

// BridgeTick is the broker-attested tick envelope.
type BridgeTick struct {
	Payload BridgeTickPayload `json:"payload"`
}

// BridgeTickPayload carries the broker-reported financials.
type BridgeTickPayload struct {
	EquityCents    int64        `json:"equity_cents"`
	BalanceCents   int64        `json:"balance_cents"`
	MarginCents    int64        `json:"margin_cents"`
	FreeMarginCents int64       `json:"free_margin_cents"`
	BrokerTime     int64        `json:"broker_time"`
	Positions      []PositionDTO `json:"positions"`
}

// TickDTO is the per-symbol tick shape.
type TickDTO struct {
	Symbol string  `json:"symbol"`
	Quote  QuoteDTO `json:"quote"`
}

// QuoteDTO carries bid/ask/timestamp.
type QuoteDTO struct {
	Bid string `json:"bid"`
	Ask string `json:"ask"`
	TS  string `json:"ts"`
}

// PositionDTO is the wire shape for an open position.
type PositionDTO struct {
	Symbol          string `json:"symbol"`
	Side            string `json:"side"`
	OpenQuantity    string `json:"open_quantity"`
	AvgEntryPrice   string `json:"avg_entry_price"`
	OpenedAt        string `json:"opened_at"`
	StopLoss        *string `json:"stop_loss,omitempty"`
	TakeProfit      *string `json:"take_profit,omitempty"`
}

// TradeDTO is the wire shape for a trade fill.
type TradeDTO struct {
	Symbol       string `json:"symbol"`
	Side         string `json:"side"`
	Price        string `json:"price"`
	Quantity     string `json:"quantity"`
	ExecutedAt   string `json:"executed_at"`
	RealizedPnl  string `json:"realized_pnl,omitempty"`
	Commission   string `json:"commission,omitempty"`
	Swap         string `json:"swap,omitempty"`
}

// RuleEntryDTO is the wire shape for a rule pack entry.
type RuleEntryDTO struct {
	ID              string  `json:"id"`
	Kind            string  `json:"kind"`
	Basis           string  `json:"basis"`
	Unit           string  `json:"unit"`
	Value           string  `json:"value"`
	ToleranceCents  *int64  `json:"tolerance_cents,omitempty"`
	EarlyWarningPct *string `json:"early_warning_pct,omitempty"`
	Priority        uint32  `json:"priority"`
	Enabled         bool    `json:"enabled"`
	ParamsJSON      string  `json:"params_json,omitempty"`
}

// RulePackResponse is the response from POST /v1/rule-packs/validate.
type RulePackResponse struct {
	ID           string `json:"id"`
	Version      uint32 `json:"version"`
	ContentHash  string `json:"content_hash"`
	Lifecycle    string `json:"lifecycle"`
	Valid        bool   `json:"valid"`
	Errors       []string `json:"errors,omitempty"`
}
