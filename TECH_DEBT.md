# Technical Debt & Limitations

## Android Client (Critical)
- **Protocol Lag**: The server now speaks a rich AI protocol (`ai_interaction`), but the Android client only understands basic success/fail messages. **Priority: High**.
- **SDUI Components**: Currently supports only basic text/buttons. Needs input fields for AI questions.

## AI / Server
- **Prompt Tuning**: The `agentPrompt.js` is V1. Needs refinement based on real-world usage data.
- **Tool Error Handling**: If the database is locked, the AI might hallucinate success. Needs better error propagation from tools.
