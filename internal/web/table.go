package web

import (
	"sync"
	"time"
)

// table is an in-memory map of random ids to values that expire. It holds
// login sessions and the state of ceremonies in flight: all of it can be lost
// on restart.
type table[T any] struct {
	mu    sync.Mutex
	items map[string]entry[T]
}

type entry[T any] struct {
	value   T
	expires time.Time
}

// put stores value under a new random id.
func (t *table[T]) put(value T, ttl time.Duration) (string, error) {
	key, err := NewToken()
	if err != nil {
		return "", err
	}

	t.mu.Lock()
	defer t.mu.Unlock()

	now := time.Now()

	if t.items == nil {
		t.items = map[string]entry[T]{}
	}

	for key, item := range t.items {
		if !item.expires.After(now) {
			delete(t.items, key)
		}
	}

	t.items[key] = entry[T]{value: value, expires: now.Add(ttl)}

	return key, nil
}

func (t *table[T]) get(id string) (T, bool) { //nolint:ireturn // T is a type parameter
	t.mu.Lock()
	defer t.mu.Unlock()

	item, ok := t.items[id]
	if !ok || !item.expires.After(time.Now()) {
		var zero T

		return zero, false
	}

	return item.value, true
}

// take is get that also forgets the entry.
func (t *table[T]) take(id string) (T, bool) { //nolint:ireturn // T is a type parameter
	value, found := t.get(id)

	t.mu.Lock()
	delete(t.items, id)
	t.mu.Unlock()

	return value, found
}
