  // Host-owned idle fallback, backported from codex-acp#593 (Apache-2.0).
  async performSteeringRequest(params) {
    const sessionState = this.getSessionState(params.sessionId);
    this.assertSteerInputSupported(params, sessionState);
    const hostOwnsFallback = params.idleBehavior === "promptRequired";
    // A pending turn may be waiting for permission. Do not hold the steer
    // response until that permission is answered; its original prompt owns it.
    const turnId = hostOwnsFallback
      ? (this.sessionIsClosing(params.sessionId) ? null : sessionState.currentTurnId)
      : await this.getSteerableTurnId(sessionState);
    if (turnId) {
      const injected = await this.injectSteerIntoActiveTurn(params, turnId, sessionState);
      if (injected) return { outcome: "injected" };
    }
    if (hostOwnsFallback) return { outcome: "promptRequired", reason: "noRunningTurn" };
    return await this.startNewTurnFromSteering(params);
  }
