# Using CCR-Rust with OpenAI SDK

CCR-Rust exposes an OpenAI-compatible `/v1/chat/completions` endpoint.
Point any OpenAI SDK client at it to route requests through your configured backends.

## How It Works

CCR-Rust auto-detects OpenAI-format requests by inspecting the request body
(presence of `messages`, `model`, etc.) and routes them through whichever
backend is configured in `~/.claude-code-router/config.json`. Any route
defined in your config — GLM, Kimi, Gemini, Minimax, or others — will work.

## Python

```python
from openai import OpenAI

client = OpenAI(
    base_url="http://localhost:3456/v1",
    api_key="unused",  # the local listener remains unauthenticated
)

response = client.chat.completions.create(
    model="glm-5.1",  # must match a model in your config.json routes
    messages=[{"role": "user", "content": "Hello"}],
)
print(response.choices[0].message.content)
```

## Node.js

```typescript
import OpenAI from "openai";

const client = new OpenAI({
  baseURL: "http://localhost:3456/v1",
  apiKey: "unused", // the local listener remains unauthenticated
});

const response = await client.chat.completions.create({
  model: "glm-5.1",
  messages: [{ role: "user", content: "Hello" }],
});
console.log(response.choices[0].message.content);
```

## Compatible Backends

Any backend with a route in your `config.json` is available. Set the `model`
parameter to the model name expected by that route. Run `ccr-rust dashboard`
to see all configured routes and their current status.
