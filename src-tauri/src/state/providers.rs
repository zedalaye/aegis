//! The provider roster and the decision model, as the commands see them
//! (PLAN 7.18, PLAN 7.19).

use super::*;

impl AppState {
    /// The default row's provider, as a session of the built-in identity with no
    /// override would get it.
    pub fn provider(&self) -> Box<dyn Provider> {
        self.build_provider(&self.binding(&Agent::builtin(), None))
    }

    /// Who answers one turn of `session_id` as `agent` (PLAN 7.19), decided per
    /// turn so a settings, identity or override change applies to the next
    /// message. Turns, handoffs and routines all come through here.
    pub fn provider_for(&self, agent: &Agent, session_id: &str) -> Box<dyn Provider> {
        let binding = self.sessions.binding_of(session_id).unwrap_or_else(|err| {
            tracing::warn!(%err, session_id, "no session to read a binding from");
            (None, None)
        });
        self.build_provider(
            &self.binding(agent, Some((binding.0.as_deref(), binding.1.as_deref()))),
        )
    }

    /// The row and model `agent` answers with, under a session's override.
    pub fn binding(&self, agent: &Agent, session: Option<(Option<&str>, Option<&str>)>) -> Binding {
        let (session_provider, session_model) = session.unwrap_or((None, None));
        store::settings::resolve(
            &self.settings.list(),
            &BindingRequest {
                identity_provider: &agent.provider_id,
                identity_model: &agent.model,
                session_provider,
                session_model,
            },
        )
    }

    /// The provider for a resolved binding.
    ///
    /// An unconfigured row gets the scripted provider. A configured row gets the
    /// real one even with no key, so the user sees `E_NO_API_KEY` rather than a
    /// silent fake
    /// ([`ProviderSettings::is_configured`](crate::store::ProviderSettings::is_configured)).
    pub(super) fn build_provider(&self, binding: &Binding) -> Box<dyn Provider> {
        let settings = binding.settings.clone();

        if !settings.is_configured() {
            return Box::new(FakeProvider::new());
        }

        let key = self.row_key(&binding.provider_id, settings.auth_kind);

        // motosan handles the CLI logins, Gemini, and an API key for
        // Anthropic's own host, whose compatibility layer drops prompt caching.
        if catalog::uses_motosan(settings.auth_kind, &settings.base_url) {
            return Box::new(SubscriptionProvider::new(settings, key, self.http.clone()));
        }

        Box::new(OpenAiProvider::new(self.http.clone(), &settings, key))
    }

    /// A row's stored key. A CLI login reads its own file, so it has none here.
    pub(super) fn row_key(&self, provider_id: &str, kind: AuthKind) -> Option<ApiKey> {
        if kind.is_cli() {
            return None;
        }
        self.secrets
            .inspect_account(&secrets::account_for(provider_id))
            .key
    }

    /// The roster and what may be shown about each key: rows from disk, key
    /// facts from one platform read per keyed row
    /// ([`SecretStore::inspect_account`](crate::secrets::SecretStore::inspect_account)).
    pub fn masked_settings(&self) -> MaskedSettings {
        // The default account answers for availability even when its row is a
        // CLI login, which never reads the store.
        let default_held = self.secrets.inspect();
        let keyring_available = default_held.keyring_available;

        let providers = self
            .settings
            .list()
            .into_iter()
            .map(|entry| {
                let kind = entry.settings.auth_kind;
                let (key_source, key_hint) = if kind.is_cli() {
                    let source = match kind {
                        AuthKind::ClaudeCli => KeySource::ClaudeCli,
                        AuthKind::CodexCli => KeySource::CodexCli,
                        AuthKind::GrokCli => KeySource::GrokCli,
                        AuthKind::ApiKey | AuthKind::Gemini => KeySource::None,
                    };
                    let hint = oauth::peek(kind).map(|peek| key_hint(peek.access_token.expose()));
                    (source, hint)
                } else {
                    let held = if entry.is_default() {
                        None
                    } else {
                        Some(
                            self.secrets
                                .inspect_account(&secrets::account_for(&entry.id)),
                        )
                    };
                    let held = held.as_ref().unwrap_or(&default_held);
                    (
                        held.source,
                        held.key.as_ref().map(|key| key_hint(key.expose())),
                    )
                };

                MaskedProvider {
                    id: entry.id,
                    label: entry.label,
                    auth_kind: kind,
                    base_url: entry.settings.base_url,
                    model: entry.settings.model,
                    max_output_tokens: entry.settings.max_output_tokens,
                    key_source,
                    key_hint,
                }
            })
            .collect();

        MaskedSettings {
            providers,
            keyring_available,
            presets: AuthKind::presets().to_vec(),
            decision: self.masked_decision(),
        }
    }

    // -----------------------------------------------------------------------
    // Decision model (PLAN 7.18)
    // -----------------------------------------------------------------------

    /// What the WebView may know about the decision client. One more platform
    /// read, for `typesafe-api-key`.
    pub(super) fn masked_decision(&self) -> MaskedDecision {
        let held = self.secrets.inspect_account(secrets::TYPESAFE_ACCOUNT);
        let settings = self.settings.decision();
        MaskedDecision {
            key_source: held.source,
            key_hint: held.key.as_ref().map(|key| key_hint(key.expose())),
            model: settings.model,
            base_url: settings.base_url,
            annotate_approvals: settings.annotate_approvals,
            default_model: store::settings::DECISION_DEFAULT_MODEL.to_owned(),
            default_base_url: store::settings::DECISION_DEFAULT_BASE_URL.to_owned(),
        }
    }

    /// The decision client as Settings stand now, or `None` without a key or
    /// an HTTP client. Built per turn, like the provider, and never from the
    /// chat key.
    pub fn decision_client(&self) -> Option<DecisionClient> {
        self.try_decision_client()
            .inspect_err(|err| {
                if *err != decision::DecisionError::NoKey {
                    tracing::warn!(%err, "no decision client");
                }
            })
            .ok()
    }

    pub(super) fn try_decision_client(&self) -> Result<DecisionClient, decision::DecisionError> {
        let key = self.secrets.inspect_account(secrets::TYPESAFE_ACCOUNT).key;
        DecisionClient::new(self.http.clone(), key, &self.settings.decision())
    }

    /// Saves the decision settings, then the key when one is given. A blank
    /// key keeps the stored one.
    pub fn save_decision(
        &self,
        model: &str,
        base_url: &str,
        annotate_approvals: bool,
        api_key: Option<&str>,
    ) -> AppResult<()> {
        self.settings
            .set_decision(model, base_url, annotate_approvals)?;
        match api_key.and_then(ApiKey::new) {
            Some(key) => self.secrets.store_account(secrets::TYPESAFE_ACCOUNT, &key),
            None => Ok(()),
        }
    }

    /// Removes the stored TypeSafe key. `AEGIS_TYPESAFE_API_KEY` is untouched.
    pub fn clear_decision_key(&self) -> AppResult<()> {
        self.secrets.clear_account(secrets::TYPESAFE_ACCOUNT)
    }

    /// One cheap question to TypeSafe, reported like a chat probe.
    pub async fn probe_decision(&self) -> ProviderProbe {
        match self.try_decision_client() {
            Ok(client) => client.probe().await,
            Err(err) => decision::unusable_probe(&err),
        }
    }

    /// The row `provider_id` names, or `E_INVALID_SETTING` on `field`.
    pub(super) fn known_row(
        &self,
        provider_id: &str,
        field: &'static str,
    ) -> AppResult<ProviderEntry> {
        self.settings
            .entry(provider_id)
            .ok_or_else(|| AppError::Settings {
                field,
                reason: format!("`{provider_id}` is not a provider in Settings"),
            })
    }

    /// Saves one row, and its key when one is given.
    ///
    /// Settings are validated and written before the key, so a rejected URL
    /// never touches the credential store. The model's output ceiling is
    /// looked up first, with the key about to be stored.
    pub async fn save_provider(
        &self,
        provider_id: &str,
        draft: &RowDraft<'_>,
        api_key: Option<&str>,
    ) -> AppResult<ProviderEntry> {
        if !self.settings.contains(provider_id) {
            return Err(AppError::ProviderNotFound {
                id: provider_id.to_owned(),
            });
        }
        let cap = self
            .model_output_cap(
                provider_id,
                draft.auth_kind,
                draft.base_url,
                draft.model,
                api_key,
            )
            .await;

        let saved = self.settings.update(
            provider_id,
            &RowDraft {
                max_output_tokens: cap,
                ..*draft
            },
        )?;
        self.store_row_key(&saved, api_key)?;
        Ok(saved)
    }

    /// Appends a row under a fresh id, and its key when one is given.
    pub async fn add_provider(
        &self,
        draft: &RowDraft<'_>,
        api_key: Option<&str>,
    ) -> AppResult<ProviderEntry> {
        let pending = api_key.and_then(ApiKey::new);
        let cap = catalog::output_cap(
            draft.auth_kind,
            draft.base_url,
            draft.model,
            self.http.as_ref(),
            pending.as_ref(),
        )
        .await;

        let added = self.settings.add(&RowDraft {
            max_output_tokens: cap,
            ..*draft
        })?;
        self.store_row_key(&added, api_key)?;
        Ok(added)
    }

    /// Files a row's key. A blank key keeps what is stored; a CLI row stores
    /// none.
    pub(super) fn store_row_key(
        &self,
        entry: &ProviderEntry,
        api_key: Option<&str>,
    ) -> AppResult<()> {
        let Some(key) = api_key.and_then(ApiKey::new) else {
            return Ok(());
        };
        if entry.settings.auth_kind.is_cli() {
            tracing::warn!(id = %entry.id, "a key sent for a CLI login was not stored");
            return Ok(());
        }
        self.secrets
            .store_account(&secrets::account_for(&entry.id), &key)
    }

    /// Removes a row's stored key. Refused for a CLI row: that login is the
    /// CLI's, not Aegis's.
    pub fn clear_provider_key(&self, provider_id: &str) -> AppResult<()> {
        let entry = self
            .settings
            .entry(provider_id)
            .ok_or_else(|| AppError::ProviderNotFound {
                id: provider_id.to_owned(),
            })?;
        if entry.settings.auth_kind.is_cli() {
            return Err(AppError::Settings {
                field: "key",
                reason: "this provider signs in through its CLI; sign out there — Aegis \
                         stores no key for it"
                    .to_owned(),
            });
        }
        self.secrets
            .clear_account(&secrets::account_for(provider_id))
    }

    /// Deletes a row, unless it is the default one or an identity or a
    /// session override still names it. Its key goes with it.
    pub fn delete_provider(&self, provider_id: &str) -> AppResult<()> {
        if provider_id != DEFAULT_PROVIDER_ID {
            let identities = self.agents.count_for_provider(provider_id);
            let sessions = self.sessions.count_for_provider(provider_id);
            if identities > 0 || sessions > 0 {
                return Err(AppError::ProviderInUse {
                    identities,
                    sessions,
                });
            }
        }

        let entry = self.settings.entry(provider_id);
        self.settings.delete(provider_id)?;

        if let Some(entry) = entry.filter(|entry| !entry.settings.auth_kind.is_cli()) {
            if let Err(err) = self.secrets.clear_account(&secrets::account_for(&entry.id)) {
                tracing::warn!(%err, id = %entry.id, "the provider is gone; its key is not");
            }
        }
        Ok(())
    }

    /// Asks one row's server whether it is reachable and the key works.
    pub async fn probe_provider(&self, provider_id: &str) -> AppResult<ProviderProbe> {
        let entry = self
            .settings
            .entry(provider_id)
            .ok_or_else(|| AppError::ProviderNotFound {
                id: provider_id.to_owned(),
            })?;
        let settings = entry.settings;
        let key = self.row_key(&entry.id, settings.auth_kind);

        // The same fork as `build_provider`: the probe must reach what a turn
        // would.
        if catalog::uses_motosan(settings.auth_kind, &settings.base_url) {
            return Ok(motosan::probe(
                settings.auth_kind,
                &settings.model,
                &settings.base_url,
                key,
                self.http.as_ref(),
            )
            .await);
        }

        Ok(openai::probe(self.http.as_ref(), &settings, key.as_ref()).await)
    }

    /// The named model's largest reply, from the provider's catalog.
    /// `pending_key`, from a form not saved yet, wins over the row's stored key.
    pub async fn model_output_cap(
        &self,
        provider_id: &str,
        kind: AuthKind,
        base_url: &str,
        model: &str,
        pending_key: Option<&str>,
    ) -> Option<u32> {
        let key = pending_key
            .and_then(ApiKey::new)
            .or_else(|| self.row_key(provider_id, kind));

        catalog::output_cap(kind, base_url, model, self.http.as_ref(), key.as_ref()).await
    }

    /// The models an authentication kind accepts, for the base URL in the form,
    /// asked with `provider_id`'s stored key.
    pub async fn list_models(
        &self,
        provider_id: &str,
        kind: AuthKind,
        base_url: &str,
    ) -> ModelCatalog {
        catalog::list(
            kind,
            base_url,
            self.http.as_ref(),
            self.row_key(provider_id, kind).as_ref(),
        )
        .await
    }
}
