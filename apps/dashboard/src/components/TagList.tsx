import { For, createSignal, type Component } from "solid-js";

interface TagListProps {
  items: string[];
  onAdd: (item: string) => void;
  onRemove: (item: string) => void;
  placeholder?: string;
}

const TagList: Component<TagListProps> = (props) => {
  const [input, setInput] = createSignal("");

  const handleAdd = () => {
    const val = input().trim();
    if (val && !props.items.includes(val)) {
      props.onAdd(val);
      setInput("");
    }
  };

  const handleKeyDown = (e: KeyboardEvent) => {
    if (e.key === "Enter") {
      e.preventDefault();
      handleAdd();
    }
  };

  return (
    <div>
      <div style={{ "flex-wrap": "wrap" }} class="flex gap-2 mb-2">
        <For each={props.items}>
          {(item) => (
            <span class="tag">
              {item}
              <span class="tag-remove" onClick={() => props.onRemove(item)}>
                ×
              </span>
            </span>
          )}
        </For>
      </div>
      <div class="flex gap-2">
        <input
          type="text"
          value={input()}
          onInput={(e) => setInput(e.currentTarget.value)}
          onKeyDown={handleKeyDown}
          placeholder={props.placeholder ?? "Add..."}
          style={{ "flex": "1" }}
        />
        <button class="btn btn-sm" onClick={handleAdd}>
          Add
        </button>
      </div>
    </div>
  );
};

export default TagList;
