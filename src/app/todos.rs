use topcoat::{
    Result,
    context::Cx,
    router::page,
    runtime::{Event, Signal, procedure, shard, signal},
    view::{View, attributes, view},
};

use crate::components::button::{ButtonSize, ButtonVariant, button};
use crate::components::input::input;
use crate::features::todo::Todo;

#[page]
pub async fn page(cx: &Cx) -> Result<impl View> {
    // The text in the input, kept in the browser.
    let title = signal(cx, String::new);
    // Increased after every change, so the list renders again with the new todos.
    let revision = signal(cx, || 0u32);

    Ok(view! {
        <main class="mx-auto max-w-2xl px-4 py-16">
            <h1 class="text-3xl font-semibold tracking-tight">"Todos"</h1>
            <form
                class="mt-4 flex gap-2"
                @submit=$(async |e: Event| {
                    e.prevent_default();
                    add(title.get()).await;
                    title.set("".to_owned());
                    revision.increment();
                })
            >
                input(
                    attrs: attributes! {
                        type="text"
                        placeholder="What needs doing?"
                        required=""
                        :value=$(title.get())
                        @input=$(|e: Event| title.set(e.target.value))
                    }
                )
                button(attrs: attributes! { type="submit" }, "Add")
            </form>
            todo_list(revision: revision)
        </main>
    })
}

/// The todo list, rendered on the server again whenever `revision` changes.
#[shard]
pub async fn todo_list(cx: &Cx, revision: Signal<u32>) -> Result<impl View> {
    // Reading the revision on the server makes the list depend on it.
    let _ = revision.get();
    let todos = Todo::list(cx).await?;

    Ok(view! {
        if todos.is_empty() {
            <p class="mt-4">"Nothing to do."</p>
        } else {
            <ul class="mt-4 space-y-2">
                for todo in todos {
                    // Browser code can only capture plain values, such as the id.
                    let id = todo.id;
                    <li class="flex items-center gap-2">
                        button(
                            variant: ButtonVariant::Outline,
                            size: ButtonSize::Sm,
                            attrs: attributes! {
                                @click=$(async |_e: Event| {
                                    toggle(id).await;
                                    revision.increment();
                                })
                            },
                            if todo.done {
                                "Undo"
                            } else {
                                "Done"
                            }
                        )
                        if todo.done {
                            <s>(&todo.title)</s>
                        } else {
                            <span>(&todo.title)</span>
                        }
                        button(
                            variant: ButtonVariant::Ghost,
                            size: ButtonSize::Sm,
                            attrs: attributes! {
                                @click=$(async |_e: Event| {
                                    remove(id).await;
                                    revision.increment();
                                })
                            },
                            "Delete"
                        )
                    </li>
                }
            </ul>
        }
    })
}

// Procedures run on the server when the browser calls them. Their arguments come from
// the browser, so validate them like any other request data.

/// Adds a todo unless its title is blank.
#[procedure]
pub async fn add(cx: &Cx, title: String) -> Result<()> {
    let title = title.trim();
    if !title.is_empty() {
        Todo::add(cx, title).await?;
    }
    Ok(())
}

/// Marks a todo as done, or as not done if it already is.
#[procedure]
pub async fn toggle(cx: &Cx, id: u64) -> Result<()> {
    Todo::toggle(cx, id).await
}

/// Removes a todo.
#[procedure]
pub async fn remove(cx: &Cx, id: u64) -> Result<()> {
    Todo::remove(cx, id).await
}
