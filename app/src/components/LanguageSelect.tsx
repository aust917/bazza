import { useState } from "react";
import { useTranslation } from "react-i18next";
import i18n, { currentLocale, displayError, languageRevision, type UiLocale } from "@/lib/i18n";
import { cmd } from "@/lib/ipc";

export function LanguageSelect() {
  const { t } = useTranslation("common");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<unknown>(null);
  async function change(locale: UiLocale) {
    const revision = languageRevision();
    setSaving(true);
    setError(null);
    try {
      const saved = await cmd.setUiLocale(locale);
      // Native events can arrive before command responses. A newer window's
      // change must win over an older, delayed save acknowledgement.
      if (languageRevision() === revision) await i18n.changeLanguage(saved);
    } catch (e) { setError(e); }
    finally { setSaving(false); }
  }
  return (
    <div className="space-y-1.5">
      <label className="flex flex-wrap items-center gap-3 text-sm font-medium">
        {t("language")}
        <select aria-label={t("language")} value={currentLocale()} disabled={saving}
          onChange={(event) => void change(event.target.value as UiLocale)}
          className="focus-ring rounded-md border border-line-2 bg-canvas px-3 py-1.5 text-ink">
          <option value="ru" lang="ru">Русский</option>
          <option value="en" lang="en">English</option>
        </select>
      </label>
      <p className="text-xs text-ink-3">{t("languageHint")}</p>
      {error != null && <p role="alert" className="text-xs text-ember">{t("languageSaveFailed")} {displayError(error)}</p>}
    </div>
  );
}
