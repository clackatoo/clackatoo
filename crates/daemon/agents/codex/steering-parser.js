  // Host-owned idle fallback, backported from codex-acp#593 (Apache-2.0).
  parseSessionSteerParams(params) {
    const sessionId = params["sessionId"];
    const prompt = params["prompt"];
    if (typeof sessionId !== "string" || !Array.isArray(prompt)) {
      throw RequestError.invalidParams();
    }
    const meta = params._meta;
    const steering = meta?.steering;
    for (const value of [meta, steering]) {
      if (value != null && (typeof value !== "object" || Array.isArray(value))) {
        throw RequestError.invalidParams();
      }
    }
    const idleBehavior = steering?.idleBehavior;
    if (idleBehavior !== undefined && idleBehavior !== "startNewTurn" && idleBehavior !== "promptRequired") {
      throw RequestError.invalidParams();
    }
    return { sessionId, prompt, idleBehavior };
  }
