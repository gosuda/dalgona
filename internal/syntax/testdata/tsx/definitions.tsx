export function Card(props: { title: string }) {
  return <div>{props.title}</div>;
}

export const Button = () => <button>ok</button>;

export interface CardProps {
  title: string;
}
