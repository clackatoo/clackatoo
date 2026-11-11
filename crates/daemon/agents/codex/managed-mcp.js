  async createSessionConfig(projectPath, additionalDirectories, mcpServers, allowNativeMcp = process.env["CLACKATOO_NATIVE_MCP"] !== "off") {
    const sessionRoots = [projectPath, ...additionalDirectories];
    const activeProvider = this.gatewayConfig ? {
      apiType: gatewayApiTypeFromConfig(this.gatewayConfig),
      baseUrl: this.gatewayConfig.config.base_url
    } : this.getNativeProviderConfig();
    logger.log("Creating session config", {
      projectPath,
      overrideActive: this.gatewayConfig !== null,
      modelProvider: this.getModelProvider(),
      apiType: activeProvider.apiType,
      baseUrl: activeProvider.baseUrl
    });
    const mergedConfig = {
      ...forceGitRootTurnDiffPaths(mergeGatewayConfig(this.config, this.gatewayConfig)),
      projects: Object.fromEntries(sessionRoots.map((root) => [root, {
        trust_level: "trusted"
      }]))
    };
    const configWithWorkspaceRoots = mergeSandboxWorkspaceWriteRoots(mergedConfig, additionalDirectories);
    const managedOnly = allowNativeMcp === false;
    const disabled = managedOnly
      ? Object.fromEntries([...await this.getConfigMcpServerNames(projectPath)].map(name => [name, {enabled: false}]))
      : {};
    if (managedOnly) {
      configWithWorkspaceRoots.mcp_servers = {...configWithWorkspaceRoots.mcp_servers, ...disabled};
      configWithWorkspaceRoots["features.apps"] = false;
    }
    if (mcpServers.length === 0) {
      return { config: configWithWorkspaceRoots, skippedMcpServers: [] };
    }
    const requestedServers = mcpServers.map((mcp) => ({
      name: sanitizeMcpServerName(mcp.name),
      server: mcp
    }));
    let serversToConfigure = requestedServers;
    if (!managedOnly && shouldDeduplicateMcpConflicts()) {
      const existingNames = await this.getConfigMcpServerNames(projectPath);
      serversToConfigure = requestedServers.filter((mcp) => !existingNames.has(mcp.name));
    }
    const skippedMcpServers = requestedServers.filter((mcp) => !serversToConfigure.includes(mcp)).map((mcp) => mcp.name);
    if (skippedMcpServers.length > 0) {
      logger.log("Skipping requested MCP servers that the Codex config already defines", {
        projectPath,
        skippedMcpServers
      });
    }
    if (serversToConfigure.length === 0) {
      return { config: configWithWorkspaceRoots, skippedMcpServers };
    }
    return {
      config: {
        ...configWithWorkspaceRoots,
        "mcp_servers": {...disabled, ...Object.fromEntries(serversToConfigure.map((mcp) => [mcp.name, {...this.createMcpSeverConfig(mcp.server), enabled: true}]))}
      },
      skippedMcpServers
    };
  }
