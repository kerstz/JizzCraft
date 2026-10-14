//! Playful one-liners sprinkled through the interface.
//!
//! They are a setting (on by default, "Playful messages" in the Library
//! menu) and are always off while discreet mode, share-safe mode or the
//! disguised identity is active: nothing cheeky may show up on a shared
//! screen. Lines are mild and translated per language; the pick rotates with
//! a counter so the same moment does not always say the same thing.

use crate::i18n::{self, Lang};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

static ENABLED: AtomicBool = AtomicBool::new(false);
static TICK: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Quip {
    Welcome,
    AgeGate,
    Scanned,
    EmptyResults,
    Buffering,
    Ended,
    NothingDownloading,
    InboxZero,
    OneHand,
    ViewSaved,
    Starting,
}

/// Called every frame from the app with the effective state.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// A line for this moment, or `None` when jokes are off.
pub fn quip(q: Quip) -> Option<&'static str> {
    if !enabled() {
        return None;
    }
    let lines = lines(i18n::current(), q);
    if lines.is_empty() {
        return None;
    }
    let n = TICK.fetch_add(1, Ordering::Relaxed);
    Some(lines[n % lines.len()])
}

/// A stable line (no rotation) for text drawn every frame.
pub fn steady(q: Quip) -> Option<&'static str> {
    if !enabled() {
        return None;
    }
    lines(i18n::current(), q).first().copied()
}

pub fn lines(l: Lang, q: Quip) -> &'static [&'static str] {
    use Quip::*;
    match (l, q) {
        // ---- English ----------------------------------------------------
        (Lang::En, Welcome) => &[
            "Take your time. Nobody's judging.",
            "Make yourself comfortable.",
            "Your secret is safe. Probably.",
        ],
        (Lang::En, AgeGate) => &["Adults only. No fibbing, you little rascal."],
        (Lang::En, Scanned) => &[
            "Found some new friends.",
            "Fresh content. Naughty, naughty.",
            "More to explore, you little rascal.",
        ],
        (Lang::En, EmptyResults) => &[
            "Nothing here. Try something else, naughty one.",
            "Hmmm… nothing matches. Get creative.",
            "No match. Your standards are too high.",
        ],
        (Lang::En, Buffering) => &[
            "Hmmm… good things come to those who wait.",
            "Patience, little rascal…",
            "Warming up…",
        ],
        (Lang::En, Ended) => &[
            "Again? You little rascal.",
            "Already? Hit replay.",
            "Shall we go again?",
        ],
        (Lang::En, NothingDownloading) => {
            &["Nothing downloading. Hands free for now.", "Quiet in here…"]
        }
        (Lang::En, InboxZero) => &["Inbox zero. Productive little rascal."],
        (Lang::En, OneHand) => &[
            "Hmmm… why only one hand?",
            "Left-hand mode. We don't ask why.",
        ],
        (Lang::En, ViewSaved) => &["Saved. Your secret is safe with us."],
        (Lang::En, Starting) => &["Hold on, rascal…", "Getting things ready…"],
        // ---- Français -----------------------------------------------------
        (Lang::Fr, Welcome) => &[
            "Prends ton temps, mon petit coquinou.",
            "Installe-toi bien. Personne ne juge.",
            "Ton secret est bien gardé. Normalement.",
        ],
        (Lang::Fr, AgeGate) => &["Réservé aux grands. Pas de mensonge, petit coquin."],
        (Lang::Fr, Scanned) => &[
            "Du nouveau contenu, mon petit coquinou va.",
            "Des nouveautés… vilain garnement.",
            "Encore plus à explorer, petit coquin.",
        ],
        (Lang::Fr, EmptyResults) => &[
            "Rien par ici. Essaie autre chose, petit coquin.",
            "Hmmmm… rien ne correspond. Sois créatif.",
            "Aucun résultat. Tu es trop exigeant.",
        ],
        (Lang::Fr, Buffering) => &[
            "Hmmmm… patience, mon petit coquinou.",
            "Tout vient à point à qui sait attendre…",
            "Ça chauffe…",
        ],
        (Lang::Fr, Ended) => &[
            "Encore ? Petit coquin va.",
            "Déjà fini ? Relance, coquinou.",
            "On remet ça ?",
        ],
        (Lang::Fr, NothingDownloading) => &[
            "Rien en cours. Les mains sont libres.",
            "Calme plat par ici…",
        ],
        (Lang::Fr, InboxZero) => &["Boîte vide. Bravo, petit coquin productif."],
        (Lang::Fr, OneHand) => &[
            "Hmmmm… pourquoi une seule main ?",
            "Mode main gauche. On ne pose pas de questions.",
        ],
        (Lang::Fr, ViewSaved) => &["Enregistré. Ton secret est bien gardé."],
        (Lang::Fr, Starting) => &["Un instant, coquinou…", "On prépare tout ça…"],
        // ---- Español ------------------------------------------------------
        (Lang::Es, Welcome) => &[
            "Tómate tu tiempo, pillín.",
            "Ponte cómodo. Nadie te juzga.",
            "Tu secreto está a salvo. Probablemente.",
        ],
        (Lang::Es, AgeGate) => &["Solo para mayores. Sin mentiras, pillín."],
        (Lang::Es, Scanned) => &[
            "Contenido nuevo. Qué travieso.",
            "Novedades… granuja.",
            "Más por explorar, pillín.",
        ],
        (Lang::Es, EmptyResults) => &[
            "Aquí no hay nada. Prueba otra cosa, travieso.",
            "Hmmm… nada coincide. Sé creativo.",
            "Sin resultados. Eres muy exigente.",
        ],
        (Lang::Es, Buffering) => &[
            "Hmmm… paciencia, pillín.",
            "Lo bueno se hace esperar…",
            "Calentando motores…",
        ],
        (Lang::Es, Ended) => &[
            "¿Otra vez? Qué pillín.",
            "¿Ya? Dale a repetir.",
            "¿Repetimos?",
        ],
        (Lang::Es, NothingDownloading) => {
            &["Nada descargando. Manos libres por ahora.", "Qué silencio…"]
        }
        (Lang::Es, InboxZero) => &["Bandeja vacía. Qué pillín tan productivo."],
        (Lang::Es, OneHand) => &[
            "Hmmm… ¿por qué solo una mano?",
            "Modo mano izquierda. No preguntamos.",
        ],
        (Lang::Es, ViewSaved) => &["Guardado. Tu secreto está a salvo."],
        (Lang::Es, Starting) => &["Un momento, pillín…", "Preparando todo…"],
        // ---- Deutsch ------------------------------------------------------
        (Lang::De, Welcome) => &[
            "Lass dir Zeit, du kleiner Schlingel.",
            "Mach es dir bequem. Niemand urteilt.",
            "Dein Geheimnis ist sicher. Wahrscheinlich.",
        ],
        (Lang::De, AgeGate) => &["Nur für Erwachsene. Nicht schummeln, Schlingel."],
        (Lang::De, Scanned) => &[
            "Neue Inhalte. Du kleiner Schlingel.",
            "Frischer Stoff… na sowas.",
            "Mehr zu entdecken, du Racker.",
        ],
        (Lang::De, EmptyResults) => &[
            "Hier ist nichts. Probier etwas anderes, Schlingel.",
            "Hmmm… nichts passt. Sei kreativ.",
            "Kein Treffer. Du bist zu anspruchsvoll.",
        ],
        (Lang::De, Buffering) => &[
            "Hmmm… Geduld, du Racker.",
            "Gut Ding will Weile haben…",
            "Es wird warm…",
        ],
        (Lang::De, Ended) => &[
            "Schon wieder? Du Racker.",
            "Schon vorbei? Nochmal abspielen.",
            "Noch eine Runde?",
        ],
        (Lang::De, NothingDownloading) => {
            &["Nichts im Download. Hände frei.", "Es ist still hier…"]
        }
        (Lang::De, InboxZero) => &["Eingang leer. Fleißiger kleiner Schlingel."],
        (Lang::De, OneHand) => &[
            "Hmmm… warum nur eine Hand?",
            "Linkshändermodus. Wir fragen nicht.",
        ],
        (Lang::De, ViewSaved) => &["Gespeichert. Dein Geheimnis ist sicher."],
        (Lang::De, Starting) => &["Moment, Schlingel…", "Wird vorbereitet…"],
        // ---- Italiano -----------------------------------------------------
        (Lang::It, Welcome) => &[
            "Prenditi il tuo tempo, birbantello.",
            "Mettiti comodo. Nessuno giudica.",
            "Il tuo segreto è al sicuro. Forse.",
        ],
        (Lang::It, AgeGate) => &["Solo per adulti. Niente bugie, birbantello."],
        (Lang::It, Scanned) => &[
            "Nuovi contenuti. Che monello.",
            "Novità fresche… birichino.",
            "Altro da esplorare, birbantello.",
        ],
        (Lang::It, EmptyResults) => &[
            "Qui non c'è niente. Prova altro, monello.",
            "Hmmm… niente corrisponde. Sii creativo.",
            "Nessun risultato. Sei troppo esigente.",
        ],
        (Lang::It, Buffering) => &[
            "Hmmm… pazienza, birbantello.",
            "Le cose belle si fanno attendere…",
            "Si scalda…",
        ],
        (Lang::It, Ended) => &[
            "Ancora? Che monello.",
            "Già finito? Premi riproduci.",
            "Rifacciamo?",
        ],
        (Lang::It, NothingDownloading) => {
            &["Nessun download. Mani libere per ora.", "Che silenzio…"]
        }
        (Lang::It, InboxZero) => &["Posta vuota. Che birbante produttivo."],
        (Lang::It, OneHand) => &[
            "Hmmm… perché una mano sola?",
            "Modalità mano sinistra. Non chiediamo.",
        ],
        (Lang::It, ViewSaved) => &["Salvato. Il tuo segreto è al sicuro."],
        (Lang::It, Starting) => &["Un attimo, birbantello…", "Prepariamo tutto…"],
        // ---- Português ----------------------------------------------------
        (Lang::Pt, Welcome) => &[
            "Toma o teu tempo, marotinho.",
            "Fica à vontade. Ninguém julga.",
            "O teu segredo está seguro. Provavelmente.",
        ],
        (Lang::Pt, AgeGate) => &["Só para adultos. Sem mentiras, marotinho."],
        (Lang::Pt, Scanned) => &[
            "Conteúdo novo. Que maroto.",
            "Novidades… traquinas.",
            "Mais para explorar, marotinho.",
        ],
        (Lang::Pt, EmptyResults) => &[
            "Aqui não há nada. Tenta outra coisa, maroto.",
            "Hmmm… nada corresponde. Sê criativo.",
            "Sem resultados. És muito exigente.",
        ],
        (Lang::Pt, Buffering) => &[
            "Hmmm… paciência, marotinho.",
            "O que é bom faz-se esperar…",
            "A aquecer…",
        ],
        (Lang::Pt, Ended) => &[
            "Outra vez? Que maroto.",
            "Já acabou? Carrega em repetir.",
            "Repetimos?",
        ],
        (Lang::Pt, NothingDownloading) => {
            &["Nada a transferir. Mãos livres por agora.", "Que silêncio…"]
        }
        (Lang::Pt, InboxZero) => &["Caixa vazia. Que maroto produtivo."],
        (Lang::Pt, OneHand) => &[
            "Hmmm… porquê só uma mão?",
            "Modo mão esquerda. Não perguntamos.",
        ],
        (Lang::Pt, ViewSaved) => &["Guardado. O teu segredo está seguro."],
        (Lang::Pt, Starting) => &["Um instante, marotinho…", "A preparar tudo…"],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_QUIPS: [Quip; 11] = [
        Quip::Welcome,
        Quip::AgeGate,
        Quip::Scanned,
        Quip::EmptyResults,
        Quip::Buffering,
        Quip::Ended,
        Quip::NothingDownloading,
        Quip::InboxZero,
        Quip::OneHand,
        Quip::ViewSaved,
        Quip::Starting,
    ];

    #[test]
    fn every_language_has_a_line_for_every_moment() {
        for l in i18n::ALL {
            for q in ALL_QUIPS {
                let v = lines(l, q);
                assert!(!v.is_empty(), "{}: {q:?}", l.code());
                assert!(v.iter().all(|s| !s.trim().is_empty() && s.len() < 90));
            }
        }
    }

    #[test]
    fn nothing_is_said_while_jokes_are_off() {
        set_enabled(false);
        assert!(quip(Quip::Welcome).is_none());
        assert!(steady(Quip::Ended).is_none());
        set_enabled(true);
        assert!(quip(Quip::Welcome).is_some());
        set_enabled(false);
    }
}
